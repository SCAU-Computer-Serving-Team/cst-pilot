# 工具调用卡片：数据到组件

本文定义一套可推广到全部工具的卡片组织方式。行为与折叠层级以[诊断卡片](../../../web/SPEC/diagnostic-cards.md)和[聊天工作台](../../../web/SPEC/chat-workspace.md#工具调用的折叠)为准；颜色、字体与尺寸以 [DESIGN.md](../../../../DESIGN.md#工具调用卡片) 和[工具调用画布](../../../../src/web/design/cst-pilot-tools.pen)为准。这里规定数据怎样进入画布、怎样落到 HeroUI v3 组件。

## 核心结构

**采用面向对象的组合方式**：每种工具的结果由对应的转换函数处理；函数产出统一的卡片数据。卡片只负责渲染。需要结构化展示的工具增加转换函数，复用卡片骨架与内容组。

```text
工具调用参数 + 工具结果
        ↓ 按工具名、scope 选转换函数
卡片数据：状态 + 一种主模板 + 有序内容组 + 原始结果
        ↓ 通用渲染器
HeroUI 卡片骨架 + 项目自有的字段行/排行行/原文块
```

| 层 | 只负责什么 | 依据 |
|---|---|---|
| 工具结果 | 保留 `toolCallId`、工具名、调用参数、`content`、`details`、`isError` | [工具返回约定](../../../tool/README.md#返回结构)；非诊断工具按自己的结构处理 |
| 转换函数 | 检查实际字段，决定主模板、组顺序、字段标签与值、提示、缺失状态 | [逐工具设计](README.md) |
| 通用卡片 | 展开/折叠、状态行、内容组、原始数据入口、浅深主题 | 画布和[诊断卡片](../../../web/SPEC/diagnostic-cards.md) |

转换函数接收原始结果，输出带明确种类的数据；界面按种类绘制，不从任意对象猜测字段含义。函数按工具名登记，内部可按 scope 分支。统一入口先处理 `isError`，未登记工具显示原始文本。

`null`、`0`、`[]`、字段缺席和 `{error}` 必须由对应工具分别解释。整次失败不能把文本当成功 JSON 解析。诊断结果优先读 `details`；其他工具按各自返回选择 `details` 或 `content`，缺少结构化条目时保留原文。诊断工具的 `content` 可能被限长，`details` 仍为完整采集数据。

## 卡片数据的最小形状

以下是前端内部的设计类型，**不改变工具返回协议**。采用带 `kind` 的联合类型，组件根据种类渲染；不需要为每个 scope 继承一张卡片。

```ts
type IconRole = "drive" | "temperature" | "warning" | "error" | "copy" | "raw";
type Metric = { label: string; value: string };
type Block =
  | { kind: "fields"; id: string; title?: string; rows: { id: string; label: string; value: string; icon?: IconRole }[] }
  | { kind: "ranking"; id: string; title?: string; rows: { id: string; name: string; metrics: Metric[] }[] }
  | { kind: "text"; id: string; text: string }
  | { kind: "image"; id: string; contentIndex: number; alt: string }
  | { kind: "notice"; id: string; status: "default" | "warning" | "danger"; text: string }
  | { kind: "commands"; id: string; items: { id: string; shell: string; admin: boolean; summary: string; command: string }[] };

type CompletedCard = {
  base: "overview" | "ranking" | "text";
  status: "success" | "degraded" | "error";
  blocks: Block[];
};
```

主模板决定第一眼看的内容：**概览组、排行组、原文块三选一**。`blocks` 可以重复添加同类组，也能附加字段组或提示；这不改变所选主模板。`sys overview` 使用强调展示，`runbook` 增加命令组，两者沿用同一骨架。等待或中断是调用状态，不从尚未到达的结果推断模板。

| 内容组 | 数据形态 | 项目组件及 HTML 语义 | 扩展办法 |
|---|---|---|---|
| 概览组 | 标题 + 字段行 | `FieldGroup`：`section` + `dl`，每行 `dt`/`dd` | 数组每项生成一组；增加字段行即可 |
| 排行组 | 标题 + 排行行 + 若干指标 | `RankingGroup`：`section` + `ol`/`li`；指标为文本 | 同一结果有多份排行时重复组；每行可追加指标 |
| 原文块 | 原文、错误或长文本 | `TextBlock`：保留换行的文本区域；命令/日志可用 `pre` | 不强行解析任意文本为 JSON |
| 图片 | `content` 中的图片块 | `ImageBlock`：读取对应 `contentIndex`，按 `mimeType` 展示 | 图片与同次调用的文字并存，不丢弃附件 |
| 提示 | 口径、部分失败、权限、截断 | `Notice`：说明文字与状态图标，必要时用 HeroUI `Alert` | 随所选模板附加；长内容允许换行 |
| 命令组 | 环境、说明、命令、复制 | 项目 `CommandGroup` + HeroUI `Button` | 仅确有命令条目的工具使用；复制参数与交付结果分别取源 |

项目组件直接消费 `Block`，不接收原始工具对象。例如字段组的结构：

```tsx
function FieldGroup({ block }: { block: Extract<Block, { kind: "fields" }> }) {
  return (
    <section aria-label={block.title}>
      {block.title && <h4>{block.title}</h4>}
      <dl>{block.rows.map((row) => (
        <div key={row.id} className="tool-field-row">
          <dt>{row.icon && <FieldIcon role={row.icon} aria-hidden="true" />}{row.label}</dt>
          <dd>{row.value}</dd>
        </div>
      ))}</dl>
    </section>
  );
}
```

排行组同理用 `ol`/`li`，一行展示名称与多个指标；原文块保留换行和长路径的折行。`FieldIcon` 是项目图标组件。图片组按索引引用原始 `content` 中的图片，不把图片字节写进展示字段。

行的 `id` 由工具数据中的稳定标识生成；重复名称时加入来源或原始序号。显示值由转换函数处理单位、精度和缺失文案。完整原值仍可从原始数据入口查看，不能用显示文本回写工具数据。

图标是前端按字段**含义**选择的角色，具体图形由画布与图标资源确定。工具返回的任意字符串不直接作为图标名。

| 位置 | 图标依据 |
|---|---|
| 状态行 | 调用中显示等待；整次错误显示失败；部分结果或降级显示警告；取消保留中断状态 |
| 组标题/字段 | 转换函数给已知语义字段指定图标，如盘符、温度；默认无图标 |
| 提示 | 由 `notice`、错误与权限说明的严重程度选提示图标；文字必须同时保留 |
| 操作 | 原始数据、复制命令等固定操作图标，不能当结果状态使用 |

## HeroUI v3 的实际落点

前端尚未实现，以下是组件对应关系与实现形状。HeroUI 管交互和容器，字段行、排行行、图标和提示的视觉按本项目画布实现。

| 画布部位 | HeroUI v3 / 原生元素 | 实现要点 |
|---|---|---|
| 单次调用的容器 | `Card`、`Card.Header`、`Card.Content`、`Card.Footer` | `variant="transparent"` 起步，项目样式覆盖间距与边框；不要给每个字段组再套一层卡片 |
| 第 2/3 层展开 | `Disclosure.Heading`、`Disclosure.Trigger`、`Disclosure.Content` | 用 `isExpanded` / `onExpandedChange` 受控；状态行只放一个展开触发器，复制按钮等操作留在展开区 |
| 调用中的指示 | `Spinner size="sm"` 或画布等待指示 | 只表示进行中，不伪造进度；中断不能显示成功 |
| 警告、失败原因 | `Alert`（`Indicator`、`Content`、`Description`） | `status` 区分严重程度；项目样式保持中性背景，语义色只落在图标上 |
| 原始数据、复制 | `Button`，`onPress` | 独立可聚焦的控件；原始数据开关有展开状态，图标按钮需可读名称 |
| 字段、排行、原文 | 原生 `section` / `dl` / `ol` / `pre` 等 | HeroUI 没有对应的诊断数据组件；由项目组件组合 |

HeroUI 的 `Card` 是非交互容器；展开行为交给 `Disclosure.Trigger`。以下 JSX 展示结果返回后的骨架，`StatusIcon`、`renderBlock` 和 `RawResult` 是项目组件：

```tsx
import { Button, Card, Disclosure } from "@heroui/react";
import { Fragment, useId } from "react";

type ToolCardProps = {
  view: CompletedCard & { toolName: string; callText: string; elapsedText: string };
  expanded: boolean;
  setExpanded: (value: boolean) => void;
  rawOpen: boolean;
  toggleRaw: () => void;
  result: unknown;
};

function ToolCallCard({ view, expanded, setExpanded, rawOpen, toggleRaw, result }: ToolCardProps) {
  const rawId = useId();
  return (
    <Card variant="transparent" className="tool-card">
      <Disclosure isExpanded={expanded} onExpandedChange={setExpanded}>
        <Card.Header>
          <Disclosure.Heading>
            <Disclosure.Trigger className="tool-card__trigger">
              <StatusIcon status={view.status} />
              <span>{view.toolName}</span><span>{view.elapsedText}</span>
              <Disclosure.Indicator />
            </Disclosure.Trigger>
          </Disclosure.Heading>
        </Card.Header>
        <Disclosure.Content>
          <Card.Content>
            <div className="tool-card__call">{view.callText}</div>
            {view.blocks.map((block) => <Fragment key={block.id}>{renderBlock(block)}</Fragment>)}
          </Card.Content>
          <Card.Footer>
            <Button variant="ghost" size="sm" onPress={toggleRaw} aria-expanded={rawOpen} aria-controls={rawId}>
              原始数据
            </Button>
          </Card.Footer>
          <div id={rawId} hidden={!rawOpen}>{rawOpen && <RawResult result={result} />}</div>
        </Disclosure.Content>
      </Disclosure>
    </Card>
  );
}
```

折叠行与卡片展开状态由浏览器保存；刷新或历史回放的默认规则见[诊断卡片](../../../web/SPEC/diagnostic-cards.md#渲染规则)。浅深主题只切换样式，卡片数据不分主题。展开内容各组间距为 12px，字段行/排行行间距为 4px；其余尺寸按画布。图标由状态或字段含义选择，不能根据随意的数值自动推测健康、危险。

## 逐工具设计

当前项目开放的工具与各自的主模板、字段映射和实测结果见[工具目录](README.md)。整次 `isError` 使用原文块；部分失败保留可用数据并标注原因。扩展工具也可能通过 `details.error` 返回失败，按对应工具的规则处理。等待返回时沿用调用中状态。

## 画布推广检查

新增一个工具形态时，先回答以下四项，再决定是否需要新增画布：

1. **主模板**：用户先看字段、排行还是原文？一个结果只选一个主模板。
2. **组与字段**：列出每个字段对应的原始路径、格式、组顺序；重复对象靠重复组，不复制卡片骨架。
3. **额外信息**：识别提示、局部错误、空值、空列表、截断、权限和取消。可用数据与失败原因同时保留。
4. **例外**：现有组无法表达的交付动作或特殊视觉强调，才增加内容组或例外画布；保持状态行、原始数据入口和折叠行为一致。

HeroUI v3 组件依据：[Card](https://heroui.com/en/docs/react/components/card)、[Disclosure](https://heroui.com/en/docs/react/components/disclosure)、[Alert](https://heroui.com/en/docs/react/components/alert)、[Button](https://heroui.com/en/docs/react/components/button)、[Spinner](https://heroui.com/en/docs/react/components/spinner)。
