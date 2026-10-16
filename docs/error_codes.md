# 场景校验诊断码（稳定契约）

> 这份表的**内容**由 `crates/quench3d-cli/src/scene_doc.rs` 的 `ErrorCode::TABLE` 生成，
> 并由单元测试 `error_codes_doc_matches_the_table` 逐格对账 —— 手改标记之间的文字会让测试红。
> 要重算整张表（一条命令，输出直接替换下面 BEGIN/END 之间的内容）：

```bash
./target/debug/quench3d schema --json \
  | python3 -c 'import json,sys; s=json.load(sys.stdin)["data"]["error_codes"]; print("| 码 | 含义（什么性质的问题） | agent 该做什么 |"); print("| --- | --- | --- |"); [print("| `%s` | %s | %s |" % (e["code"], e["meaning"], e["agent_action"])) for e in s]'
```

## 这是什么、为什么需要

`quench3d validate` 以前只给 `severity` + `path` + 中文 `message`。agent 想区分
「模型路径不存在」和「字段非法」只能去正则匹配中文文案 —— 文案一改，分支就烂。
现在每条诊断都有 `code`：

```json
{ "severity": "error", "code": "asset_not_found", "path": "models[0].path",
  "message": "找不到模型文件 \"chair.glb\"（找过：…）", "hint": "…" }
```

**契约**：

| 说的是 | 意思是 |
| --- | --- |
| `code` | 问题的**性质**（该做什么）—— 稳定、可穷举、**发布后不改名不改含义** |
| `path` | 问题的**位置**（改哪里）—— 例如 `objects[2].shape.size` |
| `message` | 给人读的细节（数字、可用名单）—— 可以改措辞，agent **不应该**依赖它做分支 |
| `hint` | 修复建议（可选） |

码表故意做得**窄**（十几个）而不是「一条诊断一个码」：232 个调用点如果各有各的码，
既没法保证稳定，agent 也没法穷举。`code` 说性质、`path` 说位置，两个合起来才是完整定位。

## 码表

<!-- BEGIN GENERATED TABLE（内容由 quench3d schema --json 生成，勿手工编辑） -->
| 码 | 含义（什么性质的问题） | agent 该做什么 |
| --- | --- | --- |
| `unknown_field` | 出现了模型/格式里不存在的字段名 | 按 hint 里的合法字段表改名或删掉；嵌套结构的父字段名也可能只是拼错 |
| `missing_field` | 缺了当前写法必需的字段（不同动作类型需要的字段不同） | 补上提示里的字段；别靠缺省猜，缺省不等于"它会自己补" |
| `invalid_type` | JSON 类型不对（该给数组给了字符串、该给数字给了布尔……） | 按提示里的期望类型改 JSON 值（注意 `[x,y,z]` 是数组，不是字符串） |
| `invalid_value` | 取值不在允许集合里：枚举拼错、格式/预设/扩展名不支持、空数组、两条轨道长度不一致 | 换成提示里列出的取值（枚举名区分大小写）；引擎不会对未知取值兜底 |
| `out_of_range` | 数值越界、非有限（NaN/Inf）、符号不对，或退化（零向量/零尺寸/上限小于下限） | 按提示的范围改数值；单位见字段说明（`rotation_deg` 是度，铰链 `limits` 是弧度） |
| `unknown_reference` | 按名字引用了不存在的东西：物体、刚体、材质、模板或相机 | 改引用名，或把被引用的东西补进对应段；hint 里通常带可用名单 |
| `duplicate_name` | 同一命名空间里名字重复（`instances` 展开之后的重名也算） | 改名；重名会让后者覆盖前者，或让引用挂到错的物体上 |
| `reserved_name` | 用了引擎保留的名字（例如相机段里的 `default`） | 换成别的名字；保留名的含义见 hint |
| `cycle` | 父子引用成环（沿 `parent` 走回自己） | 打断环：环上至少一个物体的 `parent` 要指向环外或留空 |
| `asset_not_found` | 找不到贴图/模型文件，或 `assets.search_paths` 指向不存在的目录 | 把文件放到消息里列出的候选位置之一，或用 `assets.search_paths` / `--asset-path` 加一条搜索路径 |
| `io_error` | 读写文件失败：不只是"找不到"，还包括权限、目录、编码 | 按消息里的 OS 错误处理路径/权限/编码；这不是场景语法问题，改 JSON 没用 |
| `limit_exceeded` | 超过规模上限：物体数、步数、动作数、探针数、局部光名额 | 拆成多个场景或减少数量；hint 里给了具体上限 |
| `mutually_exclusive` | 两个字段只能写一个，却同时写了（`shape`/`shapes`、`at_step`/`at_time`……） | 删掉其中一个；hint 说明同时写时哪个优先 |
| `ineffective` | 写法合法但不会生效：字段会被忽略、是死字段，或该物体根本没有刚体 | 要么删掉这个字段，要么改成会生效的写法；**别以为写了就生效** |
| `suspicious_value` | 合法但很可能不是本意：数值异常大/小，或语义容易误解（如 `scale` 只影响渲染） | 复核这个数值是否真的想要；引擎会照写执行，不会替你夹紧 |
| `unit_ambiguity` | 单位容易混：弧度与度、米与度 | 确认单位：`rotation_deg`/`motor_target_deg`/`cone_limit` 用度，铰链 `limits` 与马达速度用弧度 |
| `invalid` | 兜底码：这条诊断还没有被归类（历史诊断，或新加的检查忘了给码） | 只能读 message；如果经常撞到它，给它补一个专门的码（见 docs/error_codes.md） |
<!-- END GENERATED TABLE -->

## 覆盖情况（第 55 轮，怎么核对）

- `scene_doc.rs` 里 **232 个** `Diagnostic::error/warn` 调用点全部显式给了码
  （`Diagnostic::error_code` / `warn_code`）。两参数的老构造器仍然保留，它给的是兜底码
  `invalid` —— 只在**没法归类**时用，用来兼容围栏外的调用点与老 JSON。
- 字段级问题（未知字段 / 类型不对 / 枚举拼错 / 缺字段）在**唯一出口** `check_at` 里分类
  （`classify_serde_code`），所以结构检查不可能漏码。
- 核对命令：

```bash
# 调用点数量（应为 232）
grep -c 'Diagnostic::error_code(\|Diagnostic::warn_code(' crates/quench3d-cli/src/scene_doc.rs
# 还没归类的调用点（应为空）
grep -n 'Diagnostic::error(\|Diagnostic::warn(' crates/quench3d-cli/src/scene_doc.rs
```

**代码 ↔ 本文件的一一对应**（可复核判据，不用读源码找字符串）：

```bash
# ① 权威码集：代码里的 ErrorCode::TABLE，经 schema 暴露（17 行）
./target/debug/quench3d schema --json | python3 -c 'import json,sys
print("\n".join(sorted(e["code"] for e in json.load(sys.stdin)["data"]["error_codes"])))' > /tmp/from_code.txt
# ② 本文件码表第一列（17 行）
python3 -c 'import re
s=open("docs/error_codes.md",encoding="utf-8").read()
b=s.split("<!-- BEGIN GENERATED TABLE")[1].split("<!-- END GENERATED TABLE -->")[0]
print("\n".join(sorted(re.findall(r"^\| `([a-z_]+)` \|", b, re.M))))' > /tmp/from_doc.txt
diff /tmp/from_code.txt /tmp/from_doc.txt && echo "1:1 一致"
```

⚠️ 两个**看着自然但其实数错**的 grep（别再用）：

- `grep -oE 'code: "[a-z_.]+"' scene_doc.rs` → **0**：码不是结构体字面量，而是宏声明里的
  `Variant => "wire_name",`，全文件每码**只出现这一次**（这正是"不可能只加一半"的原因）。
- `` grep -oE '`[a-z][a-z_.]+`' docs/error_codes.md `` → **53**：反引号里除了码，还有散文里的
  标识符（`classify_serde_code`、`assets.search_paths`、`check_at`……）。只有**表格行的第一列**
  才是码。

## 守卫测试（改坏了一定会红）

| 测试 | 盯什么 |
|---|---|
| `error_code_table_is_self_consistent` | 表自身：名字唯一、snake_case、不含 `|`、serde 名一致、兜底码在位 |
| `error_codes_doc_matches_the_table` | 本文件的表 ↔ `ErrorCode::TABLE` **逐格**（码/含义/动作三列）对账 |
| `diagnostic_codes_are_stable_and_specific` | **稳定契约**：16 个非兜底码各有真实触发场景，钉死 `code` + `path`；末尾反向断言"覆盖集 == 码表 − `invalid`" |
| `every_code_in_the_table_is_emitted_by_the_source` | **死码**：码表里每个码在 `mod tests` 之前的源码里至少有一个发出点（含兜底码） |
| `serde_errors_classify_into_stable_codes` | 结构检查唯一出口的分类规则 |
| `diagnostics_without_a_code_deserialize_to_invalid` | 老 JSON 向后兼容 |
| `schema_error_codes_match_the_table` / `system_prompt_mentions_diagnostic_codes` | schema 与系统提示不漏码 |
| e2e `diagnostics_carry_stable_machine_readable_codes` | 真跑二进制：每条诊断有码、具体问题具体码、码表可查、文本输出也带码 |

**变异验证**（改坏 → 红 → 改回 → 绿，四条独立的都做过）：

| 变异 | 变红的测试 |
|---|---|
| 改码名 `asset_not_found` → `asset_missing` | 文档对账 + 稳定性契约 + e2e（3 条独立守卫） |
| 误改 `models[0].path` 的码 | 稳定性契约（文档对账**正确**地保持绿：表没变） |
| 手改本文件某一格文案 | 文档对账 |
| 停掉 `classify_serde_code` 的 unknown-field 分支 | 分类测试 |
| 摘掉 `ReservedName` 的唯一发出点（造死码） | `every_code_in_the_table_is_emitted_by_the_source` |
| 删掉 `suspicious_value` 的触发场景 | 稳定性契约（报"缺场景的码 = [suspicious_value]"） |

## 怎么加一个新码

1. 在 `scene_doc.rs` 的 `define_error_codes!` 里加一行
   `Variant => "wire_name", "含义", "agent 该做什么";`（宏会同时生成枚举、总表与
   `as_str`/`meaning`/`agent_action`，**不可能只加一半**）；
2. 重新生成上面 BEGIN/END 之间的表（用开头那条命令）；
3. **把码接到真实的校验分支上**（`Diagnostic::error_code` / `warn_code`）——没接上的话
   `every_code_in_the_table_is_emitted_by_the_source` 会红；
4. 在 `diagnostic_codes_are_stable_and_specific` 的 `covered` 列表里补一条**真实场景**
   （哪段 JSON 在哪个 `path` 上给出这个码）——没补的话那条测试末尾的反向断言会红；
5. `cargo test -p quench3d-cli -- error_code diagnostic_codes every_code_in_the_table` ——
   `scene_doc.rs` 与 `schema.rs` 各一条对账测试会同时检查本文件与 `quench3d schema --json`；
6. 在 `CHANGELOG.md` 里写一句（新增码是**对 agent 的契约变更**，不是内部重构）。

**不允许**：改已有码的名字或含义。要改就加新码、把旧码标成 deprecated 并保留映射。

## 诚实边界（这一轮没做的）

- **顶层 `error.kind` 仍然只有粗粒度几类**（`scene`/`invalid`/`environment`/`network`/`io`/
  `runtime`）。细分发生在 `diagnostics[].code` 上；`error.kind` 字段本身定义在
  `crates/quench3d-cli/src/report.rs`，本轮围栏不包含那个文件，所以没动它。
- **`RunError`（运行期错误，如渲染阶段失败）还没有码** —— 同上，`ErrorInfo` 在
  `report.rs` 里。
- 兜底码 `invalid` 理论上仍可能出现（围栏外的 `generate.rs` 两处测试夹具、以及以后
  新加的检查忘了给码）。它不是「正常码」，出现即代表该补码。
