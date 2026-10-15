# MongoDB 支持设计

- 日期:2026-10-15
- 分支:`feat/mongodb`(不在 main 上直接改)
- 状态:待审阅

## 1. 目标与范围

在 Datum 中接入 MongoDB,作为继 Redis 之后第二个非关系型数据源。

**首版范围(B 档):** 浏览 + 查询 + 单文档增删改。

| 纳入 | 不纳入(后续迭代) |
|---|---|
| 表单 / 连接串两种连接方式,含 Atlas `mongodb+srv://` | 表格内直接改字段(`$set` 单元格编辑) |
| 库 → 集合两级浏览,分页,表格 / JSON 两种视图 | JSON 树视图逐字段编辑 |
| 集合 tab 结构化过滤栏(Filter / Sort / Projection) | 索引与集合的创建 / 删除 |
| 查询 tab 支持 mongosh 语法子集 | JSON / CSV 导入导出 |
| 整篇文档编辑 / 新增 / 删除 | AI 对话生成查询、字段 schema 推断、字段补全 |
| 风险审查、只读拦截、超时 | `Driver` trait 重构(Mongo 落地后单独立项) |

**成功标准:**

1. 能连上单机、副本集、Atlas(`+srv`)三类部署;连接串中的密码不以明文写入配置文件(Local 环境除外,与现有规则一致)。
2. 打开集合即可翻页查看文档,过滤 / 排序 / 投影即时生效。
3. 任意文档"打开编辑器 → 不改动 → 保存"后,服务端数据的 BSON 类型完全不变。
4. 空过滤的批量删除 / 更新、`$out` 聚合在执行前必然经过确认弹窗。
5. 现有 PG / MySQL / SQLite / Redis 行为无回归,`cargo test --workspace` 全绿。

## 2. 架构决策

沿用现有"按库分模块"的结构:core 层新建 `mongo/` 模块,`service.rs` 的各 `match kind` 补 `DbKind::Mongo` 分支,并新增 `mongo_*` 专用入口(同 `redis_*`)。

不在本次引入 `Driver` trait:那需要先迁移四套现有驱动(103 处 `DbKind::` 引用,分布在 19 个文件),会把重构和新功能混在一起。等 Mongo 落地、有五种实现可参照后,再单独立项抽象。

**顺带的结构改进(仅限与本功能直接相关的部分):**

- `DbKind::is_relational()` 目前同时表达三种语义,Mongo 加入后答案不再一致,拆为三个能力方法(见 §3.1)。
- `tab/mod.rs` 已有 1400 多行,Mongo 的 tab 逻辑放到独立文件,`mod.rs` 只在入口处分派(见 §5)。

## 3. 数据模型与连接

### 3.1 `DbKind`

新增 `DbKind::Mongo`:label 为 `MongoDB`,scheme 为 `mongodb`,默认端口 27017;`DbKind::ALL` 扩为 5 项。

删除 `is_relational()`,替换为:

| 方法 | 含义 | Pg / MySQL / SQLite | Redis | Mongo |
|---|---|---|---|---|
| `supports_sql()` | SQL 编辑器能力:关键字大写、SQL 补全、AI 对话、导入 / 导出 SQL、ERD | ✓ | ✗ | ✗ |
| `has_columns()` | 是否加载列 schema(`fetch_columns`) | ✓ | ✗ | ✗ |
| `supports_doc_edit()` | 是否提供文档编辑 / 新增 / 删除 | ✗ | ✗ | ✓ |

现有 7 处 `is_relational()` 调用按各自的语义换成对应方法:`chat/mod.rs`、`dialogs/import.rs`(2 处)、`erd/mod.rs` → `supports_sql()`;`store.rs` → `has_columns()`;`tab/mod.rs:241`(是否设置 `browse_table`)与 `workspace/sidebar.rs:599`(导入 / 导出菜单)→ `supports_sql()`。散落各处的 `== DbKind::Redis` 判断保持不变,Mongo 走独立分派,不复用这些分支。

### 3.2 `Connection` 新字段

两个字段都带 `#[serde(default)]`,旧配置可以无损读取:

```rust
/// 仅 Mongo 使用:连接串模式下的 URI(已去除密码)。Some 即为连接串模式。
#[serde(default)]
pub uri: Option<String>,
/// 仅 Mongo 表单模式使用:认证库,None 等同 "admin"。
#[serde(default)]
pub auth_source: Option<String>,
```

### 3.3 密码处理

- **保存**:用 `mongodb::options::ConnectionString::parse` 解析 URI,取出密码交给现有的 `credentials::save_password` 处理(Local 环境明文、其余进钥匙串),再把去掉密码、仅保留用户名的 URI 写入 `uri`。`credentials.rs` 不改动。
- **连接**:`load_password` 取回密码,重新拼进 URI(按 RFC 3986 对 userinfo 做百分号编码),交给驱动。
- 去密码与拼回是纯函数,放在 `mongo/client.rs`,单测覆盖。

### 3.4 连接表单

选中 MongoDB 后,表单顶部出现"表单 / 连接串"切换:

- **表单模式**:host、port(27017)、user、password、database(默认库,可空)、认证库(占位提示 `admin`)、SSL(沿用四档 `SslMode`)。
- **连接串模式**:多行 URI 输入框 + env 选择。连接串中的 TLS 参数优先,SSL 下拉框隐藏;URI 路径中的库名作为默认库写入 `database`。
- **测试连接**:执行 `ping` 命令。

表单模式与 `SslMode` 的映射:`Disable` → `tls=false`;`Prefer` → 先尝试 TLS,失败后退回明文(与 PG 的 `prefer` 一致,不校验证书);`Require` → `tls=true` 加 `tlsAllowInvalidCertificates=true`;`VerifyFull` → `tls=true` 并用系统证书校验。

### 3.5 库与集合层级

- `service::list_databases` 返回 `listDatabaseNames` 的结果,默认过滤掉 `admin` / `local` / `config`;侧边栏提供"显示系统库"开关。
- `listDatabases` 因权限不足失败时(错误码 13 `Unauthorized`),退回只显示连接里配置的默认库,不报错;若连默认库都没配,提示"无权列出数据库,请在连接中填写默认库"。
- `service::list_objects` 列出当前库的集合与视图,视图带标记,按名称排序。

## 4. core 层:`crates/core/src/mongo/`

依赖:

```toml
mongodb = { version = "3", default-features = false, features = ["rustls-tls", "dns-resolver"] }
bson = "2"
```

不引入 OpenSSL。`dns-resolver` 用于解析 `+srv` 地址。

### 4.1 `parse.rs`:mongosh 子集解析器(纯函数)

**宽松 JSON → `bson::Document` / `Bson`**,直接产出 BSON,不经过 serde_json:

- key 可不加引号;字符串可用单引号或双引号;允许末尾逗号;支持 `//` 和 `/* */` 注释。
- 字面量:数字(整数在 i32 范围内为 Int32,超出为 Int64;含小数点或指数为 Double)、`true` / `false` / `null`。
- 构造器:`ObjectId("…")`、`ISODate("…")`、`new Date("…")`、`NumberInt(…)`、`NumberLong(…)`、`NumberDecimal("…")`、`UUID("…")`、`BinData(sub, "base64")`、`/pattern/flags`。

**语句:**

```
db.<coll>.<method>(args)[.sort(d)][.skip(n)][.limit(n)][.projection(d)]
db.getCollection("<name>").<method>(...)
show collections | show dbs
```

| 类别 | 方法 |
|---|---|
| 读 | `find(filter?, projection?)`、`findOne(filter?, projection?)`、`countDocuments(filter?)`、`distinct(field, filter?)`、`aggregate(pipeline)`、`getIndexes()` |
| 写 | `insertOne(doc)`、`insertMany(docs)`、`updateOne(f, u)`、`updateMany(f, u)`、`replaceOne(f, doc)`、`deleteOne(f)`、`deleteMany(f)` |

`.sort / .skip / .limit / .projection` 只能接在 `find` 后面,接在其他方法后面时报解析错误。

**输出:**

```rust
pub enum MongoOp {
    ShowDbs,
    ShowCollections,
    Find { coll: String, filter: Document, projection: Option<Document>, sort: Option<Document>, skip: Option<u64>, limit: Option<i64> },
    FindOne { coll: String, filter: Document, projection: Option<Document> },
    Count { coll: String, filter: Document },
    Distinct { coll: String, field: String, filter: Document },
    Aggregate { coll: String, pipeline: Vec<Document> },
    GetIndexes { coll: String },
    InsertOne { coll: String, doc: Document },
    InsertMany { coll: String, docs: Vec<Document> },
    UpdateOne { coll: String, filter: Document, update: UpdateModifications },
    UpdateMany { coll: String, filter: Document, update: UpdateModifications },
    ReplaceOne { coll: String, filter: Document, doc: Document },
    DeleteOne { coll: String, filter: Document },
    DeleteMany { coll: String, filter: Document },
}

impl MongoOp {
    /// 写操作:7 个写方法,以及管道中任意位置含 `$out` / `$merge` 的 aggregate。
    pub fn is_write(&self) -> bool;
}

pub fn parse_script(src: &str) -> Result<Vec<(Range<usize>, MongoOp)>, ParseError>;
pub fn parse_value(src: &str) -> Result<Bson, ParseError>; // 过滤栏与文档编辑器共用
```

- 多语句:按顶层 `;` 或语句之间的换行切分;选中一段时只执行选中部分。
- `ParseError { line, col, message }`。遇到不支持的语法(变量、`forEach`、`drop`、`createIndex` 等)明确报"首版不支持 X"。
- 读写判断与风险审查都基于 `MongoOp`。解析器只接受白名单语法,不存在"分析器没识别出来、服务端却会执行"的写法。

### 4.2 `convert.rs`:BSON → 展示

```rust
pub struct MongoResult {
    pub columns: Vec<String>,              // 顶层字段并集,按首次出现顺序,`_id` 固定第一列
    pub rows: Vec<Vec<Option<String>>>,    // 与 QueryResult 同形;None = 字段缺失
    pub docs: Vec<String>,                 // 每篇文档的 mongosh 风格缩进文本(JSON 视图 / 编辑器)
    pub ids: Vec<Option<Bson>>,            // 原始 `_id`,编辑 / 删除时定位
    pub truncated: bool,                   // 是否被 max_rows 截断
    pub summary: Option<String>,           // 写操作 / 标量结果的摘要
}
```

- 单元格:标量原样显示;`null` 显示为 `null` 文本,与字段缺失(`None`,灰色空格)区分;`ObjectId` 显示为 `ObjectId("…")`(Hash 色块);日期显示为 ISO-8601;嵌套的对象 / 数组用紧凑单行 mongosh 文本,超过 200 字符截断,双击在 `cell_dialog` 里看全文。
- `to_shell(&Bson, pretty: bool) -> String`:输出的语法恰好能被 `parse_value` 解析,类型标注齐全(Int64 → `NumberLong(…)`,Decimal128 → `NumberDecimal("…")`,Double 的整数值写成 `1.0` 以保留类型)。
- **往返不变式**:对所有支持的 BSON 类型,`parse_value(to_shell(b)) == b`。

### 4.3 `client.rs`:`MongoPool`

结构同 `RedisPool`:`id → mongodb::Client`,`Client` 内部自带连接池。

- 连接选项:`server_selection_timeout = 5s`、`connect_timeout = 10s`、`app_name = "Datum"`。
- 方法:`connect`、`disconnect`、`ping`、`list_databases`、`list_collections`、`find_page(db, coll, filter, sort, projection, skip, limit)`、`estimated_count`、`count(filter)`、`execute(db, &MongoOp, max_rows)`、`replace_one(db, coll, id, doc)`、`insert_one(db, coll, doc)`、`delete_one(db, coll, id)`。
- 所有读操作都设置 `max_time`,默认 60s,由 `MongoPool` 的字段持有,测试中可以调小;`find` 与 `aggregate` 按 `max_rows` 截断结果并设置 `truncated`。
- `AppState` 新增 `mongo_pool: MongoPool`。

### 4.4 `commands.rs` 与 `service.rs`

- `service.rs` 中已有的 `connect` / `disconnect` / `list_objects` / `list_databases` / `test_connection` 补上 `DbKind::Mongo` 分支。
- `query`、`table_detail`、`commit_edits`、`import_sql`、`create_table_ddl`、`fetch_columns`、`fetch_erd_schema`、`export_database` 对 Mongo 返回 `ErrorKind::Mongo`("MongoDB 不支持此操作"),正常流程靠 UI 的能力判断不会调到这些函数。
- 新增 `mongo_find_page`、`mongo_count`、`mongo_execute`、`mongo_replace_one`、`mongo_insert_one`、`mongo_delete_one`,这些都是薄转发。

## 5. UI 层

### 5.1 文件划分

| 文件 | 职责 |
|---|---|
| `tab/mongo.rs` | `refresh_mongo`、`run_mongo`、分页状态、视图切换;`WorkTab` 新增一个 `mongo: Option<MongoTabState>` 字段 |
| `tab/mongo_filter.rs` | 集合 tab 顶部的 Filter / Sort / Projection 过滤栏 |
| `tab/doc_view.rs` | JSON 视图(文档卡片列表) |
| `dialogs/doc_editor.rs` | 文档编辑 / 新增弹窗 |

`tab/mod.rs` 只在 `new_table`、`run`、`refresh` 和渲染入口处,判断 `db_kind == Mongo` 后转发到上面这些文件。

### 5.2 侧边栏

- 层级:连接 → 库 → 集合,复用现有的库层级。
- 集合右键菜单:打开、新建查询(预填 `db.<coll>.find({})`)、插入文档、复制名称、刷新。
- 连接右键菜单不显示导入 / 导出 SQL(由 `supports_sql()` 控制)。

### 5.3 集合 tab

- **过滤栏**:Filter `{…}`、Sort `{…}`、Projection `{…}` 三个单行输入框,加"应用 / 重置"按钮,回车也执行;用 `parse_value` 解析,失败时在输入框下方标红并给出位置。
- **视图**:默认表格(复用 `tab/grid.rs` 的排序、复制、单元格弹窗),右上角可切 JSON 视图;切换不重新查询,当前选择在 tab 内记住。
- **分页**:每页 50 / 100 / 500 条,默认 100;上一页 / 下一页用 skip + limit 实现。没有过滤条件时总数用 `estimatedDocumentCount`;有过滤条件时显示"计数"按钮,点了才执行 `countDocuments`。
- **工具栏**:新增、编辑选中、删除选中。只有带 `_id` 的行才能编辑和删除,projection 把 `_id` 排除掉时按钮置灰并提示原因。

### 5.4 查询 tab

- 编辑器语言为 `javascript`(用于高亮),placeholder 是 `db.users.find({ age: { $gt: 18 } })`;不做 SQL 关键字大写和 SQL 补全。
- 补全:输入 `db.` 后弹出当前库的集合名,复用补全面板。
- 执行:
  - 读操作的结果用和集合 tab 相同的表格 / JSON 视图展示;`countDocuments`、`distinct` 这类标量结果显示为单列。
  - 写操作显示摘要:matched / modified / deleted / inserted 计数,以及 `insertedId(s)`。
  - 多条语句依次执行;结果区展示最后一条读操作的结果,所有语句的摘要汇总到消息区;任一条失败就停止,并报告是第几条语句失败。
- 写进执行历史,和 SQL tab 的做法一致。

### 5.5 文档编辑弹窗

- 大号代码编辑器,内容为 `to_shell(doc, pretty = true)`,下方实时显示解析是否通过,解析失败时"保存"按钮置灰。
- **编辑保存**:执行 `replaceOne({_id: 原_id}, 新文档)`。新文档里的 `_id` 和原值不同时拒绝保存,提示"不能修改 _id";`matchedCount == 0` 时提示"文档已被删除或 _id 已变化",并刷新当前页。
- **新增**:模板为 `{\n  \n}`,省略 `_id` 时由服务端生成;成功后刷新当前页。
- **删除**:任何环境都弹出确认框,确认后执行 `deleteOne({_id})`。

## 6. 安全与错误处理

### 6.1 风险审查

在 `sql/risk.rs` 中新增 `analyze_mongo(ops: &[MongoOp]) -> Vec<RiskFinding>`,复用 `confirm_destructive` / `RiskGate`:

| 操作 | 级别 |
|---|---|
| `deleteMany` / `updateMany`,过滤条件为空 | Danger |
| `aggregate` 含 `$out` | Danger |
| `deleteMany` / `updateMany`,带过滤条件 | Warning |
| `deleteOne` / `updateOne` / `replaceOne`,过滤条件为空 | Warning |
| `aggregate` 含 `$merge` | Warning |

"过滤条件为空"指解析后得到的 `Document` 为空,所以 `{}`、`{ }`、带注释的空对象都会命中。文档编辑弹窗里的保存不走风险审查(用户已经在编辑器里看到了要写入的完整内容);删除始终要确认。

### 6.2 只读拦截

沿用 `WorkTab::read_only_next`:为 true 时,含任何 `is_write()` 操作的脚本会在客户端被直接拒绝,提示"只读模式下不能执行写操作"。

**限制:** MongoDB 没有类似 PG `default_transaction_read_only` 的会话级只读开关,这里只做客户端拦截,不提供数据库层面的保证。真正的只读要靠只授予 `read` 角色的数据库账号。

### 6.3 超时

| 项 | 值 | 目的 |
|---|---|---|
| `serverSelectionTimeout` | 5s | 地址写错或网络不通时尽快报错 |
| `connectTimeout` | 10s | 同上 |
| `maxTimeMS`(读操作) | 60s | 查询 tab 没有取消功能,防止慢查询一直挂着 |

`maxTimeMS` 超时(错误码 50 `MaxTimeMSExpired`)时提示"查询超过 60 秒已终止,请增加过滤条件或索引"。

### 6.4 错误呈现

- `AppError` 新增 `ErrorKind::Mongo`。
- 解析错误:显示行号、列号和说明,在编辑器里定位到出错位置。
- 服务端错误:显示 `code codeName: 原文消息`;13 `Unauthorized`、11000 `DuplicateKey`、50 `MaxTimeMSExpired`、`ServerSelectionTimeout` 附带本地化提示。
- 结果被截断时,状态栏提示"已截断至 N 条"。
- 所有新增文案都要补齐 i18n 三份语言文件(en / zh-CN / zh-TW)。

## 7. 测试

### 7.1 单元测试(纳入 `cargo test --workspace`,CI 必跑)

- **parse.rs**:宽松 JSON 的各种写法与全部构造器;每个支持的方法至少一条正常用例;链式调用;`getCollection`;多语句切分;选中执行;错误用例(不支持的方法、括号不配对)及行列号的准确性。
- **is_write**:7 个写方法;`aggregate` 的 `$out` / `$merge` 出现在管道末尾或中间;普通 `aggregate` 不算写。
- **analyze_mongo**:§6.1 风险表的每一行,以及空过滤条件的几种变体。
- **convert.rs**:列并集的顺序、`_id` 固定第一列、缺失字段与 `null` 的区分、截断;**往返测试**覆盖 Int32、Int64、Double(含整数值)、Decimal128、Date、ObjectId、Binary / UUID、Regex、Bool、Null、嵌套文档、嵌套数组。
- **连接**:URI 去密码 / 拼回(含特殊字符的密码)、`+srv` 保留、不带 `uri` / `auth_source` 的旧配置 JSON 能反序列化、`SslMode` 映射。
- **DbKind**:5 种库 × 3 个能力方法的真值表。

### 7.2 集成测试(`crates/core/tests/mongo.rs`,默认 ignore)

用 `DATUM_MONGO=端口` 开启,本地用 `docker run -d -p 27017:27017 mongo:7` 起库。

覆盖:ping;列库与集合;`find_page` 分页与 filter / sort / projection;增 → 查 → `replaceOne` → 读回比对 BSON 类型 → 删;`read_only_next` 下 `$out` 被拦截;只授权单库的用户触发 `listDatabases` 退回;`maxTimeMS` 生效(把 `max_time` 调成 1ms,对一个插入了 10 万篇文档的集合执行带 `$regex` 的全表扫描,断言返回错误码 50)。

### 7.3 手动验收清单

1. 表单模式连本地单机;连接串模式连本地;连接串模式连 Atlas `+srv`(TLS)。
2. 非 Local 环境保存连接后,检查配置文件里的 `uri` 不含密码。
3. 集合 tab:过滤 / 排序 / 投影、翻页、计数、表格与 JSON 视图切换。
4. 编辑一篇包含 Int64、Decimal128、Date、嵌套数组的文档,不改动直接保存,用 mongosh 确认类型没变。
5. 新增(省略 `_id`)、删除(弹出确认框)。
6. 查询 tab:读、写、多语句、`deleteMany({})` 弹出 Danger 确认。
7. PG / MySQL / SQLite / Redis 各执行一次连接 + 查询,确认没有回归。
8. 用 snapshot 机制补两张截图(集合 tab、查询 tab),放进三份 README 的导览节。

## 8. 交付方式

- 所有提交都在 `feat/mongodb` 分支上,不直接改 main。
- 合入方式(PR 或本地 merge)等功能完成后由用户决定。
