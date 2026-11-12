// 编辑器一侧的「让 AI 修复 / 解读执行计划」:左侧两个按钮模拟编辑器里的入口。
import { Text, View } from "nsh";
import { Button } from "nsh/base";
import { askAiExplainPlan, askAiFixError, planText } from "../index";
import { mountChat } from "./harness";

mountChat({ delay: 5 }, undefined, () => (
  <View class="gap-2 p-4">
    <Button testID="trigger-fix" onPress={() => askAiFixError("SELECT nme FROM users", 'column "nme" does not exist')}>
      <Text>fix</Text>
    </Button>
    <Button
      testID="trigger-plan"
      onPress={() =>
        askAiExplainPlan(
          "SELECT * FROM orders WHERE total > 10",
          planText(["QUERY PLAN"], [["Seq Scan on orders  (cost=0.00..35.50 rows=680 width=16)"], ["  Filter: (total > 10)"]]),
        )
      }
    >
      <Text>plan</Text>
    </Button>
  </View>
));
