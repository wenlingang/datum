// 执行计划树:每个节点一行(缩进表示层级),占比条是该节点自身成本占总成本的比例;
// 自身成本最高的节点标「最耗时」,全表扫描标「全表扫描」。ANALYZE 后显示实际耗时与行数。

import { Scroll, Text, View } from "nsh";
import { t } from "nsh/i18n";
import type { Plan, PlanNode } from "../lib/types";
import { FS, MONO } from "../ui/text";

interface Row {
  node: PlanNode;
  depth: number;
}

function flatten(n: PlanNode, depth: number, out: Row[]) {
  out.push({ node: n, depth });
  for (const c of n.children) flatten(c, depth + 1, out);
}

const fmt = (n: number | null, digits = 1) =>
  n === null ? "—" : n >= 1000 ? Math.round(n).toLocaleString() : n.toFixed(n < 10 ? digits : 0);

export function PlanView(props: { plan: Plan }) {
  const rows = () => {
    const out: Row[] = [];
    if (props.plan.tree) flatten(props.plan.tree, 0, out);
    return out;
  };
  const total = () => props.plan.tree?.cost ?? null;
  // 有成本时按自身成本找最耗时;ANALYZE 时按实际耗时更准
  const hottest = () => {
    let best: PlanNode | null = null;
    let score = 0;
    for (const r of rows()) {
      const s = props.plan.analyzed ? selfMs(r.node) : (r.node.selfCost ?? 0);
      if (s > score) {
        score = s;
        best = r.node;
      }
    }
    return best;
  };
  const share = (n: PlanNode) => {
    if (props.plan.analyzed && props.plan.tree?.actualMs) {
      return Math.min(100, (selfMs(n) / props.plan.tree.actualMs) * 100);
    }
    const tot = total();
    return tot && n.selfCost !== null ? Math.min(100, (n.selfCost / tot) * 100) : 0;
  };
  return (
    <Scroll testID="plan-tree" class="min-h-0 flex-1" contentClass="px-3 py-2 gap-0.5">
      <If when={props.plan.analyzed && props.plan.totalMs !== null}>
        <Text class={`${FS.xs} pb-1 text-muted-foreground`}>
          {t("plan.total_ms", { ms: fmt(props.plan.totalMs, 2) })}
        </Text>
      </If>
      <Each of={rows()}>
        {(r) => (
          <View
            testID={`plan-node:${r().node.label}`}
            class="flex-row items-start gap-3 rounded-(--radius) py-1.5 pr-2 hover:bg-list-hover"
            classList={{ "bg-warning/10": hottest() === r().node }}
            style={{ paddingLeft: 8 + r().depth * 20 }}
          >
            <View class="min-w-0 flex-1 gap-0.5">
              <View class="flex-row flex-wrap items-center gap-1.5">
                <Text class={`${FS.sm} font-bold text-foreground`}>{r().node.label}</Text>
                <If when={hottest() === r().node && (rows().length > 1)}>
                  <Text class={`${FS["2xs"]} rounded-full bg-warning px-1.5 font-bold text-background`}>{t("plan.hottest")}</Text>
                </If>
                <If when={r().node.fullScan}>
                  <Text class={`${FS["2xs"]} rounded-full border border-warning px-1.5 font-bold text-warning`}>{t("plan.full_scan")}</Text>
                </If>
              </View>
              <If when={r().node.detail}>
                <Text class={`${FS.xs} ${MONO} text-muted-foreground line-clamp-2`}>{r().node.detail}</Text>
              </If>
              <If when={total() !== null || props.plan.analyzed}>
                <View class="h-1 w-full max-w-[360px] rounded-full bg-raised">
                  <View
                    class="h-1 rounded-full"
                    classList={{ "bg-warning": hottest() === r().node, "bg-primary": hottest() !== r().node }}
                    style={{ width: `${Math.max(1, share(r().node))}%` }}
                  />
                </View>
              </If>
            </View>
            <View class="w-[230px] shrink-0 flex-row justify-end gap-4">
              <If when={r().node.cost !== null}>
                <Stat label={t("plan.cost")} value={fmt(r().node.cost)} />
              </If>
              <If when={r().node.rows !== null}>
                <Stat label={t("plan.rows")} value={fmt(r().node.rows, 0)} />
              </If>
              <If when={r().node.actualMs !== null}>
                <Stat label={t("plan.actual")} value={`${fmt(r().node.actualMs, 2)} ms`} />
              </If>
            </View>
          </View>
        )}
      </Each>
    </Scroll>
  );
}

/** ANALYZE 下节点自身耗时 = 实际耗时 - 子节点实际耗时。 */
function selfMs(n: PlanNode): number {
  const kids = n.children.reduce((s, c) => s + (c.actualMs ?? 0), 0);
  return Math.max(0, (n.actualMs ?? 0) - kids);
}

function Stat(props: { label: string; value: string }) {
  return (
    <View class="items-end">
      <Text class={`${FS["2xs"]} text-fg-faint`}>{props.label}</Text>
      <Text class={`${FS.xs} ${MONO} text-foreground`}>{props.value}</Text>
    </View>
  );
}
