// ER 图 tab:工具条 + 画布。
//
// 画布 = 一张屏幕坐标的 Canvas(点阵背景、连线、箭头)
//      + 绝对定位的表节点(只渲染视口内的;缩得很小时只画表名)
//      + 最上层的 Canvas(选中边的列配对)。
// 交互:滚轮 / 双指平移;⌘/Ctrl + 滚轮以指针为中心缩放;捏合缩放(手势中原生变换预览,
// 松手并入缩放);拖空白平移、拖表移动;点表高亮关联;双击开表;搜索高亮并移到命中处。

import { Canvas, computed, createAnimatedValue, effect, Text, untracked, View, type JSX } from "nsh";
import { t } from "nsh/i18n";
import type { ErdNode } from "../lib/types";
import { conns } from "../store/conns";
import { openTable } from "../tab";
import { Btn, color, Icon, IconButton, TextField } from "../ui/kit";
import { FS } from "../ui/text";
import { intersects, LOD_SIMPLE, NODE_HEADER_H, NODE_PAD_Y, NODE_ROW_H, routeEdge, wheelPoints, worldView, type Pt } from "./geom";
import {
  applyPendingFit,
  expand,
  fitTo,
  load,
  moveNode,
  nodeState,
  panBy,
  rebuild,
  setFocus,
  setSearch,
  setView,
  toggleInferred,
  zoomBy,
  type ErdModel,
} from "./model";

/** 两次点击间隔在此之内算双击(ms)。 */
const DOUBLE_CLICK_MS = 350;

export function ErdView(props: { m: ErdModel }) {
  const m = props.m;
  if (!untracked(m.schema) && !untracked(m.loading)) void load(m);
  // 列数据晚到(后台拉取)时按新尺寸就地更新,不整图重排
  effect(() => {
    conns.columns(m.connId);
    conns.tables(m.connId);
    untracked(() => rebuild(m, true));
  });
  return (
    <View testID="erd" class="flex-1 bg-background">
      <Toolbar m={m} />
      <Board m={m} />
    </View>
  );
}

// ------------------------------------------------------------ 工具条

function Toolbar(props: { m: ErdModel }) {
  const m = props.m;
  const stats = () => {
    const g = m.graph();
    if (!g.nodes.length && m.loading()) return "…";
    const n = g.edges.filter((e) => e.inferred).length;
    const base = t("erd.stats", { tables: g.nodes.length, edges: g.edges.length });
    return n ? base + t("erd.stats_inferred", { n }) : base;
  };
  const noEdges = () => !m.loading() && !m.error() && m.graph().nodes.length > 0 && !m.graph().edges.length;
  const empty = () => !m.graph().nodes.length;
  return (
    <View class="h-10 shrink-0 flex-row items-center gap-2.5 border-b border-border bg-panel px-3.5">
      <If when={m.focus()}>
        <Btn small variant="outline" icon="undo-2" label={t("erd.back_to_all")} onPress={() => setFocus(m, null)} testID="erd-back" />
      </If>
      <Text class={`${FS.xs} font-mono text-fg-faint`}>{stats()}</Text>
      <If when={noEdges()}>
        <Text class={`${FS.xs} text-fg-faint`}>{m.focus() ? t("erd.no_edges_table") : t("erd.no_edges_db")}</Text>
      </If>
      <View class="flex-1" />
      <If when={m.loading()}>
        <Icon name="loader-circle" size={14} color="fg-faint" />
      </If>
      <TextField
        testID="erd-search"
        class="h-6 w-56"
        value={m.search()}
        onChange={({ value }) => setSearch(m, value)}
        placeholder={t("erd.search_placeholder")}
      />
      <Btn
        small
        variant={m.inferred() ? "secondary" : "outline"}
        label={t("erd.inferred")}
        tooltip={t("erd.inferred_tooltip")}
        onPress={() => toggleInferred(m)}
        testID="erd-inferred"
      />
      <View class="flex-row gap-1">
        <Btn
          small
          variant={m.viewMode() === "struct" ? "secondary" : "outline"}
          label={t("erd.view_struct")}
          disabled={empty()}
          onPress={() => setView(m, "struct")}
          testID="erd-view-struct"
        />
        <Btn
          small
          variant={m.viewMode() === "er" ? "secondary" : "outline"}
          label={t("erd.view_er")}
          disabled={empty()}
          onPress={() => setView(m, "er")}
          testID="erd-view-er"
        />
      </View>
      <IconButton icon="refresh-cw" tooltip={t("erd.refresh_tooltip")} onPress={() => void load(m)} />
    </View>
  );
}

// ------------------------------------------------------------ 画布

interface Drag {
  /** 拖动的表;null = 平移 */
  table: string | null;
  /** 上一次的表面坐标 */
  last: Pt;
  moved: boolean;
}

/** 移动超过它(点)才算拖动,否则仍是点击。 */
const DRAG_SLOP = 3;

function Board(props: { m: ErdModel }) {
  const m = props.m;
  // 捏合:手势中原生驱动的倍数(以画布中心为原点变换预览),松手后并入 zoom 并归 1
  const pinch = createAnimatedValue(1);
  let drag: Drag | null = null;
  /** 手指按下的表:节点的 onTouchStart 先于画布的(由内向外冒泡)记下它 */
  let grabbed: string | null = null;
  /** 刚拖动过:吞掉随后的那次点击 */
  let dragged = false;

  // 拖动走触摸事件:桌面上鼠标拖动同时也是触摸;用表面坐标(pageX/Y)求增量,与视图变换无关
  const onTouchStart = (e: { pageX: number; pageY: number }) => {
    drag = { table: grabbed, last: { x: e.pageX, y: e.pageY }, moved: false };
    grabbed = null;
    dragged = false;
  };
  const onTouchMove = (e: { pageX: number; pageY: number }) => {
    if (!drag) return;
    const dx = e.pageX - drag.last.x;
    const dy = e.pageY - drag.last.y;
    if (!drag.moved && Math.abs(dx) + Math.abs(dy) < DRAG_SLOP) return;
    drag.last = { x: e.pageX, y: e.pageY };
    drag.moved = true;
    dragged = true;
    if (drag.table) moveNode(m, drag.table, dx, dy);
    else panBy(m, dx, dy);
  };
  const onTouchEnd = () => {
    drag = null;
  };

  /** 视口内的节点(带边距),平移时只增删进出视野的那几个。 */
  const visible = computed(() => {
    const w = worldView(m.viewport(), m.size());
    const rects = m.rects();
    return m.graph().nodes.filter((n) => {
      const r = rects[n.table];
      return r && intersects(w, r.x, r.y, r.x + r.w, r.y + r.h);
    });
  });

  effect(() => {
    m.size();
    m.rects();
    untracked(() => applyPendingFit(m));
  });

  return (
    <View
      testID="erd-board"
      class="relative min-h-0 flex-1 overflow-hidden bg-editor"
      onLayout={({ bounds }) => m.size.set({ width: bounds.size.width, height: bounds.size.height })}
      onWheel={(e) => {
        const d = wheelPoints(e.delta, e.deltaMode);
        if (e.modifiers.platform || e.modifiers.control) zoomBy(m, Math.exp(-d.y / 200), e.position);
        else panBy(m, -d.x, -d.y);
      }}
      pinch={{ scale: pinch }}
      onPinchEnd={async () => {
        const s = await pinch.getValue();
        pinch.set(1);
        if (s > 0 && Math.abs(s - 1) > 0.001) zoomBy(m, s);
      }}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
      onTouchCancel={onTouchEnd}
      onPress={() => {
        if (dragged) dragged = false;
        else m.selected.set(null);
      }}
    >
      <View style={{ position: "absolute", left: 0, top: 0, right: 0, bottom: 0, scale: pinch }}>
        <Edges m={m} />
        <View style={{ position: "absolute", left: m.viewport().pan.x, top: m.viewport().pan.y }}>
          <Each of={visible()} key={(n) => n.table}>
            {(n) => (
              <NodeView
                m={m}
                node={n}
                grab={(table) => (grabbed = table)}
                consumeDrag={() => {
                  const d = dragged;
                  dragged = false;
                  return d;
                }}
              />
            )}
          </Each>
        </View>
        <EdgeLabels m={m} />
      </View>
      <Overlay m={m} />
      <Controls m={m} />
    </View>
  );
}

/** 加载中 / 失败 / 空库的居中提示。 */
function Overlay(props: { m: ErdModel }) {
  const m = props.m;
  return (
    <If when={m.error() || (m.loading() && !m.graph().nodes.length)}>
      <View class="absolute inset-0 items-center justify-center gap-3 p-6">
        <If when={m.error()} else={<Icon name="loader-circle" size={24} color="fg-faint" />}>
          {(e) => (
            <>
              <Icon name="circle-alert" size={28} color="destructive" />
              <Text class={`${FS.sm} text-fg-soft`}>{t("erd.load_failed", { error: e() })}</Text>
              <Btn small variant="outline" icon="refresh-cw" label={t("erd.retry")} onPress={() => void load(m)} />
            </>
          )}
        </If>
      </View>
    </If>
  );
}

function Controls(props: { m: ErdModel }) {
  const m = props.m;
  return (
    <View class="absolute bottom-3 right-3 flex-row items-center gap-0.5 rounded-(--radius) border border-border bg-panel p-0.5 shadow-sm">
      <IconButton icon="zoom-out" tooltip={t("erd.zoom_out")} onPress={() => zoomBy(m, 1 / 1.2)} />
      <Text testID="erd-zoom" class={`${FS.xs} w-10 text-center font-mono text-fg-soft`}>
        {`${Math.round(m.viewport().zoom * 100)}%`}
      </Text>
      <IconButton icon="zoom-in" tooltip={t("erd.zoom_in")} onPress={() => zoomBy(m, 1.2)} />
      <IconButton icon="maximize" tooltip={t("erd.fit_view")} onPress={() => fitTo(m, null)} />
    </View>
  );
}

// ------------------------------------------------------------ 连线

function Edges(props: { m: ErdModel }) {
  const m = props.m;
  return (
    <Canvas
      style={{ position: "absolute", left: 0, top: 0, right: 0, bottom: 0 }}
      draw={(cx, { width, height }) => {
        const { zoom: z, pan } = m.viewport();
        const rects = m.rects();
        const hits = m.hits();
        const sel = m.selected();
        const faint = color("fg-faint");
        const primary = color("primary");

        // 点阵背景;缩小时点距翻倍,避免一帧画上万个点
        let gap = 22 * z;
        while (gap < 16) gap *= 2;
        cx.fillStyle = color("hairline");
        const ox = ((pan.x % gap) + gap) % gap;
        const oy = ((pan.y % gap) + gap) % gap;
        for (let y = oy; y < height; y += gap) for (let x = ox; x < width; x += gap) cx.fillRect(x, y, 1.2, 1.2);

        const view = worldView(m.viewport(), { width, height });
        // 缩得很小时虚线看不清:改用实线
        const dashOk = z >= 0.5;
        const arrow = Math.min(9, Math.max(4, 7 * z));
        for (const e of m.graph().edges) {
          const s = rects[e.source];
          const d = rects[e.target];
          if (!s || !d) continue;
          const route = routeEdge(s, d);
          const xs = route.map((p) => p.x);
          const ys = route.map((p) => p.y);
          if (!intersects(view, Math.min(...xs), Math.min(...ys), Math.max(...xs), Math.max(...ys))) continue;
          const lit = !hits && !!sel && (sel === e.source || sel === e.target);
          const dim = hits ? !(hits.has(e.source) && hits.has(e.target)) : !!sel && !lit;
          const pts = route.map((p) => ({ x: pan.x + p.x * z, y: pan.y + p.y * z }));
          cx.globalAlpha = dim ? 0.25 : 1;
          cx.strokeStyle = lit ? primary : faint;
          cx.fillStyle = lit ? primary : faint;
          cx.lineWidth = lit ? 2 : 1.4;
          cx.setLineDash(e.inferred && dashOk ? [6, 4] : []);
          cx.beginPath();
          cx.moveTo(pts[0].x, pts[0].y);
          for (const p of pts.slice(1)) cx.lineTo(p.x, p.y);
          cx.stroke();
          // 箭头指向被引用表
          const tip = pts[pts.length - 1];
          cx.setLineDash([]);
          cx.beginPath();
          cx.moveTo(tip.x, tip.y);
          cx.lineTo(tip.x - arrow, tip.y - arrow * 0.6);
          cx.lineTo(tip.x - arrow, tip.y + arrow * 0.6);
          cx.closePath();
          cx.fill();
        }
        cx.globalAlpha = 1;
      }}
    />
  );
}

/** 点选表时相连边的列配对:贴在折线中段,画在节点之上(单独一层,不挡点击)。 */
function EdgeLabels(props: { m: ErdModel }) {
  const m = props.m;
  return (
    <Canvas
      style={{ position: "absolute", left: 0, top: 0, right: 0, bottom: 0, pointerEvents: "none" }}
      draw={(cx) => {
        const sel = m.selected();
        if (!sel || m.hits()) return;
        const { zoom: z, pan } = m.viewport();
        const rects = m.rects();
        cx.font = "11px monospace";
        for (const e of m.graph().edges) {
          if (e.source !== sel && e.target !== sel) continue;
          const s = rects[e.source];
          const d = rects[e.target];
          if (!s || !d) continue;
          const route = routeEdge(s, d);
          const mid = route[route.length / 2 - 1];
          const lines = e.label.split("\n");
          const w = Math.max(...lines.map((t) => cx.measureText(t).width)) + 12;
          const h = lines.length * 15 + 6;
          const x = pan.x + mid.x * z + 6;
          const y = pan.y + mid.y * z - h / 2;
          cx.fillStyle = color("popover");
          cx.strokeStyle = color("primary");
          cx.lineWidth = 1;
          cx.beginPath();
          cx.roundRect(x, y, w, h, 4);
          cx.fill();
          cx.stroke();
          cx.fillStyle = color("popover-foreground");
          lines.forEach((t, i) => cx.fillText(t, x + 6, y + 3 + 11 + i * 15));
        }
      }}
    />
  );
}

// ------------------------------------------------------------ 表节点

function NodeView(props: { m: ErdModel; node: () => ErdNode; grab: (table: string) => void; consumeDrag: () => boolean }) {
  const m = props.m;
  const table = () => props.node().table;
  const r = () => m.rects()[table()];
  const z = () => m.viewport().zoom;
  const state = () => nodeState(m, table());
  const selected = () => m.selected() === table();
  let lastPress = 0;

  const onPress = () => {
    if (props.consumeDrag()) return;
    const now = Date.now();
    if (now - lastPress < DOUBLE_CLICK_MS) {
      lastPress = 0;
      openTable(m.connId, table());
      return;
    }
    lastPress = now;
    m.selected.set(selected() ? null : table());
  };

  return (
    <View
      testID={`erd-node:${table()}`}
      onPress={onPress}
      onTouchStart={() => props.grab(table())}
      class="overflow-hidden border bg-panel shadow-sm"
      style={{
        position: "absolute",
        left: (r()?.x ?? 0) * z(),
        top: (r()?.y ?? 0) * z(),
        width: (r()?.w ?? 0) * z(),
        height: (r()?.h ?? 0) * z(),
        borderRadius: 8 * z(),
        opacity: state().dim ? 0.25 : 1,
        borderColor: color(state().hit || selected() ? "primary" : "border"),
      }}
    >
      <If when={z() >= LOD_SIMPLE} else={<SimpleNode node={props.node} z={z} />}>
        <NodeHeader m={m} node={props.node} z={z} />
        <View style={{ paddingTop: NODE_PAD_Y * z(), paddingBottom: NODE_PAD_Y * z() }}>
          <Each of={props.node().columns} key={(c) => c.name}>
            {(c) => (
              <Row z={z}>
                <View style={{ width: 12 * z() }} class="shrink-0">
                  <If when={c().isPk || c().isFk}>
                    <Icon name={c().isPk ? "key-round" : "link-2"} size={11 * z()} color={c().isPk ? "data" : "fg-faint"} />
                  </If>
                </View>
                <Text class="line-clamp-1 min-w-0 flex-1 font-mono text-foreground" style={{ fontSize: 12 * z() }}>
                  {c().name}
                </Text>
                <Text class="line-clamp-1 shrink-0 text-muted-foreground" style={{ fontSize: 11 * z() }}>
                  {c().dataType}
                </Text>
              </Row>
            )}
          </Each>
          <If when={!props.node().columns.length}>
            <Row z={z}>
              <Text class="italic text-fg-faint" style={{ fontSize: 12 * z() }}>
                {t("erd.no_key_columns")}
              </Text>
            </Row>
          </If>
          <If when={props.node().hiddenCount > 0}>
            <View
              testID={`erd-more:${table()}`}
              onPress={() => expand(m, table())}
              class="flex-row items-center hover:bg-list-hover"
              style={{ height: NODE_ROW_H * z(), paddingLeft: 10 * z(), paddingRight: 10 * z(), gap: 5 * z() }}
            >
              <Icon name="chevron-down" size={11 * z()} color="fg-faint" />
              <Text class="text-fg-faint" style={{ fontSize: 11 * z() }}>
                {t("erd.more_columns", { n: props.node().hiddenCount })}
              </Text>
            </View>
          </If>
        </View>
      </If>
    </View>
  );
}

function Row(props: { z: () => number; children: JSX.Element }) {
  const z = props.z;
  return (
    <View class="flex-row items-center" style={{ height: NODE_ROW_H * z(), paddingLeft: 10 * z(), paddingRight: 10 * z(), gap: 6 * z() }}>
      {props.children}
    </View>
  );
}

function NodeHeader(props: { m: ErdModel; node: () => ErdNode; z: () => number }) {
  const z = props.z;
  const n = props.node;
  return (
    <View
      class="flex-row items-center border-b border-border bg-raised"
      style={{
        height: NODE_HEADER_H * z(),
        paddingLeft: 10 * z(),
        paddingRight: 10 * z(),
        gap: 6 * z(),
        borderTopWidth: n().isFocus ? 2 : 0,
        borderTopColor: color("primary"),
      }}
    >
      <Icon name="table-2" size={12 * z()} />
      <Text class="line-clamp-1 min-w-0 flex-1 font-mono font-semibold text-foreground" style={{ fontSize: 13 * z() }}>
        {n().table}
      </Text>
      <If when={!n().isFocus}>
        <View testID={`erd-focus:${n().table}`} onPress={() => setFocus(props.m, n().table)} hitSlop={4}>
          <Icon name="crosshair" size={12 * z()} />
        </View>
      </If>
    </View>
  );
}

/** 细节层级:缩得很小时只画表名(字号有下限),不再为成千个字段行排版文字。 */
function SimpleNode(props: { node: () => ErdNode; z: () => number }) {
  const z = props.z;
  return (
    <View class="flex-1 bg-raised" style={{ paddingLeft: Math.max(2, 10 * z()), paddingRight: Math.max(2, 10 * z()), paddingTop: Math.max(1, 8 * z()) }}>
      <Text class="line-clamp-1 font-mono font-semibold text-foreground" style={{ fontSize: Math.max(9, 13 * z()) }}>
        {props.node().table}
      </Text>
    </View>
  );
}
