# strkeymapindex：散 key ndarray 的坐标 key 布局

> 本文是 `strkeymapindex` 这一 XValue kind 的权威定义：它的语义、它的元素 key 形态、以及各后端与上层（kvlang layout/runtime、json 扩展）必须遵守的契约。
> 本文取代并作废旧文档《XValue 类型系统与 JSON 全类型 key 布局》中关于散 key 数组 key 形态的全部约定。

## 1. 两种 ndarray

kvspace 有且只有两种 ndarray，二者的类型表达式同为 `[d0,d1,...]kind`，`ndim`/`dims` 同在 XValueHead 的 kindexpr 中：

| | compact ndarray | 散 key ndarray |
|---|---|---|
| kind | 元素 kind（`float32`/`int64`/…） | `strkeymapindex` |
| 存储 | 单 XValue，元素连续打包在 body | 目录标记 XValue + 每元素一个独立 key |
| 元素约束 | 必须定宽（bool/int/uint/float/char） | 任意，可变长、可异构、可嵌套 |
| 变长 | 否，dims 固定 | 是，可增删元素 |
| 元素寻址 | body 内 row-major 偏移 | 元素 key |

两者的 `xv.numel` / `xv.dim` / `xv.shape` / `xv.at` / `xv.set` 语义完全一致，差别只在存储形态。`array.scatter` / `array.compact` 是两者之间的转换。

## 2. 元素 key = 坐标段

设 `m` 的 kindexpr 为 `[d0,d1,...,d(n-1)]strkeymapindex`，则 `n = ndim ≥ 1`，其元素 key 为：

```
m·[s0,s1,...,s(n-1)]
```

- 坐标段整体是**父目录 `m·` 下的一个成员名**，不是多级路径。成员分隔符为 `·`（U+00B7）。
- 坐标个数**恒等于 ndim**，不足或超出即非法。
- 坐标是十进制非负整数或小数（如 `12.24`），`,` 分隔，**无空格**，`[` `]` 包围。

```
一维 [3]strkeymapindex      m·[0]      m·[1]      m·[2]
二维 [2,3]strkeymapindex    m·[0,0]    m·[0,1]  … m·[1,2]
三维 [2,2,2]strkeymapindex  m·[0,0,0]  m·[0,0,1] …
小数一维                    m·[12.24]  m·[0.5]  …
```

一维**没有例外**：是 `m·[0]`，不是 `m·0`。

## 3. ndim ≥ 1 是硬约束

`strkeymapindex` 恒有维度。`ndim == 0` 的 `strkeymapindex` **不合法**，编解码两侧均须拒绝——无维度的字符串键容器是 `objindex`，不是本 kind。

由此两个 kind 的分工无重叠：

| kind | dims | 元素 key | 用途 |
|---|---|---|---|
| `objindex` | 无（ndim=0） | `m·<name>` | 命名成员字典，键是任意字符串 |
| `strkeymapindex` | ndim ≥ 1 | `m·[s0,...]` | 散 key ndarray，键是整数/小数坐标 |

## 4. key 形态字面自明

坐标段以 `[` 开头，而 `objindex` 成员名禁止 `[`（§6），因此：

```
m·[0]    数组第 0 元素
m·0      名为 "0" 的命名成员
```

二者字面可分。反向映射（如 json.to）**不需要**读父节点 kind 才能判断子键是元素还是成员。旧布局中 `m·0` 两义、必须靠父节点 kind 消歧的问题不再存在。

## 5. 嵌套：规整走 dims，不规整走元素

- **规整多维**一律扁平为单层坐标段：`[2,3]strkeymapindex` 落 6 个 key `m·[0,0]`…`m·[1,2]`，**不产生任何中间目录节点**。
- **不规整（ragged）**结构靠「元素本身又是 strkeymapindex」表达：`m·[0]` 是一个一维 map 目录，其元素为 `m·[0]·[0]`、`m·[0]·[1]`。

扁平化消除了旧布局的中间层记账节点。旧布局中 `m·0·0` 要求 `m·0·` 是一个目录 XValue，而该中间层若无人显式写入，`backend.rs` 会兜底建成 `objindex`——二维数组的中间层 kind 因此是错的。扁平后中间层不存在，该缺陷随之消失。

### JSON 数组的映射

`json.from` 产生的数组**一律是一维 map**，嵌套 JSON 数组对应「元素本身是 map」，逐层各自一维。不对 JSON 数组做规整性探测、不自动折叠成多维。理由：JSON 数组天然 ragged，规整性探测会让 `[[1,2],[3,4]]` 与 `[[1,2],[3]]` 落成两种结构，往返不稳定。

多维扁平 dims 供 kvlang 显式声明的 ndarray（tensor 场景）使用。

## 6. 成员名字符约束

**objindex 成员名**禁止：`/` `·` `[` `]` `\n` `\r` `\0`、`‥`(U+2025)、`…`(U+2026)、ASCII 控制字符（<0x20）、空串。禁 `[` 保证了 §4 的字面可分。`.` 已放开——小数/含点字符串可作 key。

**strkeymapindex 坐标段**必须完全匹配 `\[([0-9]+(\.[0-9]+)?)(,([0-9]+(\.[0-9]+)?))*\]`（非负整数或小数），其余一律非法。

违反者由写入方报错拒绝，不做转义、不静默丢弃。

## 7. dims 与成员的关系

- `dims` 是**逻辑形状**，存于目录标记 XValue 的 kindexpr。
- 成员名列表（body `[4B count LE][name\n...]`）是**实际存在的元素**。
- 二者可不一致：坐标可缺席（元素被删、或从未写入），此时该坐标读为 `None`。`count` 是实际成员数，不是 `prod(dims)`。
- 一维 map 追加元素时 `dims[0]` 随之增长；多维 map 的 dims 由创建者给定。

## 8. list 顺序

`strkeymapindex` 的 `list` 按坐标 **row-major 数值升序**返回：先比 s0，相等再比 s1，依此类推；数值比较而非字节序（`[10]` 排在 `[2]` 之后）。

`objindex` / `index` 不保证顺序。

## 9. 各后端

| 后端 | 元素落盘 | 坐标段可行性 |
|---|---|---|
| redis | key 为完整路径字符串 | `scan_keys` 已不用 Redis glob，主动规避 `[` `]` 元字符误匹配 |
| fs | `m·` 目录下的文件名 `[0,1]` | Linux 文件名允许 `[` `]` `,` |
| shm (kvspace-c) | ART 树字节串 | 无字符限制 |

fs 后端当前对 `·` 结尾目录一律返回 `objindex`（`fs/kvspace.rs` `dir_value`），`strkeymapindex` 的 kind 与 dims 在该后端存不住，需修。扁平化后 map 只有一层目录，修复点收敛为一处。

## 10. 破坏性

不向后兼容，不留别名，不做兼容读取。以下形态一律废弃：

- `m[0]`（kvlang runtime 旧散 key 形态，无点号）
- `m.0`（json 扩展旧形态、旧文档规定形态，`.` 成员分隔符时代）
- `m.0.0`（旧多维嵌套形态）

旧形态落盘数据不做迁移。当前唯一合法形态为 `m·[s0,s1,...]`。
