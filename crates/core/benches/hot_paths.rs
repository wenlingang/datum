//! 热路径基准:大库补全、ER 图布局、查询结果本地排序 / 筛选、Redis 值格式识别、流式导出行写入。
//! 运行:cargo bench -p datum-core;只看某一项:cargo bench -p datum-core -- completion

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use datum_core::completion::sql_completions_with;
use datum_core::erd::{build_graph, layout, GraphOptions};
use datum_core::grid::{filter_indices, sort_order, Cell, Conj, FilterOp, FilterRow};
use datum_core::rds::decode::{decode, Format};
use datum_core::schema::{ColumnBrief, ColumnsByTable, ErdSchema, ForeignKey};
use std::collections::HashMap;

/// 有 `tables` 张表、每张 `cols` 列的库;每张表的 `t{i-1}_id` 指向前一张表。
fn schema(tables: usize, cols: usize) -> (Vec<String>, ColumnsByTable, Vec<ForeignKey>) {
    let names: Vec<String> = (0..tables).map(|i| format!("table_{i:05}")).collect();
    let mut columns = ColumnsByTable::new();
    let mut fks = vec![];
    for (i, t) in names.iter().enumerate() {
        let mut list = vec![ColumnBrief { name: "id".into(), data_type: "bigint".into() }];
        list.extend((1..cols).map(|c| ColumnBrief { name: format!("col_{c}"), data_type: "text".into() }));
        if i > 0 {
            list.push(ColumnBrief { name: "parent_id".into(), data_type: "bigint".into() });
            fks.push(ForeignKey {
                constraint: format!("fk_{i}"),
                table: t.clone(),
                column: "parent_id".into(),
                ref_table: names[i - 1].clone(),
                ref_column: "id".into(),
                inferred: false,
            });
        }
        columns.insert(t.clone(), list);
    }
    (names, columns, fks)
}

fn rows(n: usize) -> Vec<Vec<Cell>> {
    (0..n)
        .map(|i| {
            vec![
                Some(i.to_string()),
                Some(format!("name {}", (i * 7919) % n)),
                if i % 10 == 0 { None } else { Some(format!("{}.{}", i % 1000, i % 97)) },
            ]
        })
        .collect()
}

fn completion(c: &mut Criterion) {
    let (tables, columns, fks) = schema(3000, 20);
    let text = "SELECT * FROM table_01500 t JOIN ";
    c.bench_function("completion: JOIN 候选(3000 张表 × 20 列)", |b| {
        b.iter(|| sql_completions_with(black_box(text), text.len(), &tables, &columns, &fks, false))
    });
    let text = "SELECT t.co FROM table_01500 t";
    c.bench_function("completion: 列名(3000 张表 × 20 列)", |b| {
        b.iter(|| sql_completions_with(black_box(text), 11, &tables, &columns, &fks, false))
    });
}

fn erd(c: &mut Criterion) {
    let (tables, columns, fks) = schema(1000, 12);
    let er = ErdSchema { fks, pk_by_table: Default::default() };
    let none = HashMap::new();
    let opts = GraphOptions { focus: None, collapsed: &none, full: &none };
    let graph = build_graph(&tables, &columns, &er, &opts);
    c.bench_function("erd: 整库布局(1000 张表)", |b| b.iter(|| layout(black_box(&graph))));
}

fn grid(c: &mut Criterion) {
    let data = rows(100_000);
    let columns = vec!["id".to_string(), "name".to_string(), "amount".to_string()];
    c.bench_function("grid: 10 万行按文本列排序", |b| b.iter(|| sort_order(black_box(&data), 1, false)));
    c.bench_function("grid: 10 万行按数值列排序", |b| b.iter(|| sort_order(black_box(&data), 2, true)));
    let filters = vec![FilterRow {
        id: 1,
        col: "name".into(),
        op: FilterOp::Contains,
        value: "99".into(),
        path: vec![],
        json_type: None,
    }];
    c.bench_function("grid: 10 万行本地筛选(包含)", |b| {
        b.iter(|| filter_indices(black_box(&data), &columns, &filters, Conj::And))
    });
}

fn redis_decode(c: &mut Criterion) {
    let json = format!("[{}]", (0..20_000).map(|i| format!("{{\"id\":{i},\"v\":\"x{i}\"}}")).collect::<Vec<_>>().join(","));
    c.bench_function("redis: 识别并格式化 ~500KB JSON", |b| b.iter(|| decode(black_box(json.as_bytes()), Format::Auto)));
    let binary: Vec<u8> = (0..512 * 1024).map(|i| (i * 31 % 251) as u8).collect();
    c.bench_function("redis: 识别 512KB 二进制", |b| b.iter(|| decode(black_box(&binary), Format::Auto)));
}

criterion_group!(benches, completion, erd, grid, redis_decode);
criterion_main!(benches);
