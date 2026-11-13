# strata 面试讲解手册（Stage 1）

> 这份文档按「教学顺序」逐个文件讲 strata 第一阶段的代码。每个文件分三层：
> **(a) 骨架**（这个文件在整体里干什么、有哪些部件）→ **(b) 为什么**（设计取舍、替代方案）→ **(c) 语法**（文件里第一次出现的 Rust 语法）。
> 力气主要花在 (a)(b)；(c) 只解释看代码会卡住的地方。术语第一次出现时附英文。
> 所有数字都来自 `BENCHMARK.md` 和 `results/`，面试时以那里为准。

---

## 0. 全局地图

```
                 ┌──────────── strata-core（引擎，纯计算，无 I/O）────────────┐
 文本 ──► Analyzer（分词器/分析器）──► InMemoryIndexBuilder ──build()──► InMemorySegment
                                                                        │  实现 SegmentReader trait
 查询文本 ──► Analyzer ──► Query（词袋 bag-of-words）──► Searcher ──────┘
                                                        │ BM25 打分 + top-k 小顶堆
                                                        ▼
                                                    Vec<Hit>
 strata-eval：BEIR 数据加载 → 跑全部测试查询 → 写 TREC run 文件 → 指标（nDCG/Recall/MRR）
              → pytrec_eval 交叉验证（scripts/crosscheck_pytrec.py）
 strata-bench：同样的流程跑 Tantivy（参照系，不是引擎依赖）
```

一句话介绍：**strata 是一个用 Rust 从零写的混合检索（hybrid search，词法 lexical + 向量 vector）数据库，第一阶段完成了分析器、内存倒排索引、BM25、BEIR 评测框架，并且在 BEIR 上逐位复现了 Anserini（学术界 BM25 基准实现）的结果。**

阅读顺序：`types.rs → analyzer.rs → porter.rs → index/mod.rs → index/memory.rs → scoring.rs → search.rs → tests/search_vs_brute_force.rs → beir.rs → metrics.rs → harness.rs → strata-eval main.rs → strata-mem.rs → strata-bench → scripts/`

---

## 1. `crates/strata-core/src/types.rs` —— 强类型 ID

**(a) 骨架**：三个「新类型」（newtype）：`DocId(u32)`、`SegmentId(u32)`、`ShardId(u16)`。

**(b) 为什么**
- `DocId` 是**段内稠密 ID**（dense, segment-local）：0, 1, 2, …。稠密才能把「每篇文档的长度」存成 `Vec<u32>`、用下标 O(1) 取；第二阶段做 posting 的差值编码（delta encoding）时，ID 小且递增，压缩率才高。数据集自己的 ID（如 `"MED-10"`）单独存，只在返回结果时查一次。
- `SegmentId` / `ShardId` 第一阶段用不到，但提前定义：第二阶段多段（segment）存储、第七阶段分片（shard）时，函数签名不用大改。
- 为什么不用裸 `u32`？编译器能阻止「把段 ID 当文档 ID 传」这种 bug，而运行时零开销（newtype 编译后就是一个 u32）。

**(c) 语法**
- `pub struct DocId(pub u32);` —— 元组结构体（tuple struct），`.0` 取里面的值。
- `#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]` —— 让编译器自动实现这些 trait（类似 Java 自动生成 equals/hashCode/compareTo）。`Copy` 表示按位复制、赋值后原变量仍可用（小的纯数据才加）。
- `impl fmt::Display for DocId` —— 给类型实现 trait（接口），这里是「怎么打印」。

---

## 2. `crates/strata-core/src/analyzer.rs` —— 文本分析

**(a) 骨架**
流水线（pipeline），对齐 Lucene 的 `EnglishAnalyzer`（Anserini 用的就是它）：
1. 分词（tokenize）：Unicode UAX #29 词边界（`unicode-segmentation` 的 `unicode_words()`）
2. 去英文所有格（possessive）`'s`
3. 小写（lowercase，Unicode 感知）
4. 去停用词（stopwords）：Lucene 的 33 个英文停用词
5. 词干化（stemming）：默认经典 Porter（我们自己移植的），可选 Porter2 / 不做

- `AnalyzerConfig`：纯数据配置，可序列化（serde）。
- `Analyzer`：从配置构建，内部持有停用词 `HashSet` 和词干器。
- `analyze()` 返回 `Vec<String>`；`analyze_into()` 往调用方传入的缓冲区里追加，循环里复用内存。

**(b) 为什么**
- **为什么要和 Lucene 一模一样？** 要和 Anserini 发表的数字比，「同样的词」是前提。验证：scifact 上我们的总词数 838,125，Anserini 的 Lucene 索引是 838,128（差 3 个）。
- **为什么 UAX #29？** 这是 Unicode 官方的「词边界」标准，Lucene 的 `StandardTokenizer` 也实现它。比「按空格+标点切」强：`3.14`、`1,000`、`john's` 都保持为一个词，中文按单字切。
- **为什么停用词表这么小？** BM25 的 IDF 本来就会让「the」这种词权重接近 0，停用词主要是省空间、提速，而不是提质量。消融实验（ablation）里关掉停用词，nDCG 变化很小（见 BENCHMARK.md）。
- **为什么配置要可序列化？** 第二阶段把配置写进每个段的元数据，查询时用同一套分析——「索引时和查询时分析不一致」是搜索引擎经典的静默 bug（silent bug）。
- **取舍**：`analyze` 每个词分配一个 `String`，简单但有分配开销；`analyze_into` 至少复用了外层 Vec。分析占了索引时间的大头（BENCHMARK 里有 analysis-only 时间），是以后优化点（比如 rayon 并行分析）。

**(c) 语法**
- `pub enum Stopwords { None, LuceneEnglish, Custom(Vec<String>) }` —— Rust 的 enum 是「带数据的联合体」（tagged union / sum type），每个变体可以带不同的数据。
- `match &config.stopwords { ... }` —— 模式匹配，必须覆盖所有变体（穷尽性检查，exhaustive），漏了编译不过。
- `..AnalyzerConfig::simple()` —— 结构体更新语法：其余字段从另一个值拿。
- `fn analyze(&self, text: &str) -> Vec<String>` —— **借用**（borrowing）：`&self`、`&str` 只读借用，不拿走所有权（ownership）；返回值是新分配的、调用方拥有的。
- `Option<String>` —— 「可能没有值」，代替 null。`normalize` 返回 `None` 表示这个词被过滤掉。
- `if let Some(term) = self.normalize(word) { ... }` —— 只关心 `Some` 时的简写 match。
- `fn strip_possessive(word: &str) -> &str` —— 返回输入的**子切片**，零拷贝。完整写法是 `fn<'a>(word: &'a str) -> &'a str`（生命周期 lifetime：输出活得和输入一样久），只有一个输入引用时可以省略（lifetime elision）。
- `word.char_indices().rev()` —— 迭代器（iterator）：带字节偏移的字符，倒序。Rust 字符串是 UTF-8，`’` 占 3 字节，所以必须用字节偏移切片。
- `stemmer.stem(&lowered).into_owned()` —— `stem` 返回 `Cow<str>`（copy-on-write：没变就借用，变了才分配），`into_owned()` 统一变成 `String`。
- `#[cfg(test)] mod tests` —— 只在 `cargo test` 时编译的测试模块。

---

## 3. `crates/strata-core/src/porter.rs` —— 经典 Porter 词干器

**(a) 骨架**：把 Lucene 的 `PorterStemmer.java` 一步步移植：`cons`（是否辅音）、`m`（VC 序列数，衡量词干「长度」）、`ends`（是否以某后缀结尾并记录位置 j）、`setto`/`r`（替换后缀）、`step1`…`step6`。对外只有 `porter_stem(&str) -> String`。

**(b) 为什么**
- `rust-stemmers` 只有 Snowball English（Porter2），而 Lucene/Anserini 用的是 1980 年的原版 Porter。两者对部分词结果不同（`generously`：Porter→`gener`，Porter2→`generous`）。
- **实测证明这一步有用**：换成 Porter 后，scifact/fiqa/scidocs 的 nDCG@10、R@100、R@1000 都和 Anserini 四位小数完全一致（见 BENCHMARK.md「Anserini 对照」）。只用 Porter2（精确长度）时 nDCG@10 差 0.0004–0.0049。
- **怎么保证移植正确？** 用 Porter 官网的 23,531 个词的官方词表和期望输出做测试（`testdata/porter_*.txt`），全部一致。
- 保留 Java 的函数名，方便逐行对照原实现——面试时可以说「我刻意让代码可审计（auditable）」。

**(c) 语法**
- `struct Stemmer { b: Vec<char>, k: usize, j: isize }` —— `j` 用有符号 `isize`，因为后缀可能就是整个词，j = -1。`as usize` / `as isize` 是显式数值转换，Rust 不做隐式转换。
- `match self.b[i] { 'a' | 'e' | ... => false, 'y' => ..., _ => true }` —— `|` 匹配多个值，`_` 兜底。
- `matches!(ch, 'l' | 's' | 'z')` —— 宏，返回「是否匹配这个模式」的 bool。
- `let rules: &[(&str, &str)] = match ... { 'a' => &[("ational", "ate"), ...], _ => &[] };` —— 切片（slice）引用，指向编译期常量数组；用数据表代替 Java 里一长串 if。
- `(0..=self.j).any(|i| !self.cons(i as usize))` —— 闭包（closure）`|i| ...` + 迭代器适配器 `any`。`0..=j` 是闭区间。
- `include_str!("../testdata/porter_voc.txt")` —— 编译期把文件内容嵌进二进制，测试不依赖运行时路径。

---

## 4. `crates/strata-core/src/index/mod.rs` —— 倒排索引抽象

**(a) 骨架**
- `Posting { doc: DocId, tf: u32 }`：倒排表（postings list）里的一项：「词在文档 doc 里出现 tf 次」。
- `CollectionStats { num_docs, total_tokens }`：BM25 需要的集合统计量，`avg_doc_len()`。
- `trait SegmentReader`：**搜索代码只依赖这个接口**：`num_docs`、`doc_len`、`doc_freq`、`postings(term)`、`external_id` 等。

```
term dictionary（词典）      postings（倒排表）
"bm25"  ───────────────►  [(0,2), (7,1)]
"index" ───────────────►  [(0,1), (3,4), (7,2), (9,1)]
doc lengths: [12, 40, 7, ...]     collection stats: N, total tokens
```

**(b) 为什么**
- **为什么是 trait？** 第二阶段会加 mmap 的磁盘段（on-disk segment），只要实现同一个 trait，BM25 和查询引擎一行不改。这是典型的「依赖接口而不是实现」。
- **为什么 `CollectionStats` 和段分开？** 多段 / 多分片时必须用**全局**统计量算 IDF，否则同一篇文档放在不同段里分数不同。所以 `Searcher::with_stats` 允许外部传入。（第七阶段 scatter-gather 时的「global IDF」就是这个问题。）
- **为什么 postings 必须按 doc_id 升序？** 第二阶段差值编码、第三阶段 WAND 跳表（skip）都依赖有序。

**(c) 语法**
- `type Postings<'a>: Iterator<Item = Posting> where Self: 'a;` —— **关联类型**（associated type），而且带生命周期参数，叫 GAT（generic associated type）。意思：「每个实现自己决定 postings 迭代器是什么类型，并且这个迭代器不能比段活得久」。
  - 为什么不用 `Box<dyn Iterator>`？那是动态分发（dynamic dispatch，虚函数），每个 posting 一次间接调用；关联类型是静态分发，编译器能内联，热循环更快。代价是 trait 不能当 `dyn SegmentReader` 用（不是 object-safe），以后多段可以用泛型或 enum 解决。
- `fn stats(&self) -> CollectionStats { ... }` —— trait 里的**默认方法**，实现者白拿，也可以覆盖。
- `mod memory; pub use memory::{...};` —— 声明子模块并重新导出（re-export），外部可以写 `strata_core::index::InMemorySegment`。

---

## 5. `crates/strata-core/src/index/memory.rs` —— 内存倒排索引

**(a) 骨架**：两阶段。
1. `InMemoryIndexBuilder`（可变）：`HashMap<String, Vec<Posting>>`，`add_document` 一篇篇加。
2. `build()` 冻结成 `InMemorySegment`（不可变）：词排序 + 所有 postings 拼成一个大数组（CSR 布局，compressed sparse row）：

```
terms:        ["apple", "bm25", "rust"]     （有序，二分查找）
term_offsets: [0, 2, 6, 7]                  （长度 = 词数 + 1）
postings:     [p0 p1 | p2 p3 p4 p5 | p6]
```

`add_document` 的核心：分析 → 排序 tokens → `chunk_by` 把相同词分组 → 每组就是 (term, tf) → 追加到这个词的 posting list。因为 doc_id 递增分配，每个 list 天然有序。

**(b) 为什么**
- **为什么 build 之后不可变？** 不可变数据天然线程安全（多线程读不用锁），也正好对应第二阶段「不可变磁盘段 + 后台合并」（Lucene/Tantivy 的 segment 架构，LSM 思想）。
- **为什么 CSR 一个大数组而不是 `Vec<Vec<Posting>>`？** 每个 Vec 头 24 字节 + 一次堆分配；29k 个词就是 29k 次分配。一个大数组更省内存、对缓存更友好，而且和磁盘格式同构（postings 文件 + offsets）。实测：内存里每个 posting 平均 9.3–10.7 字节（BENCHMARK 的 memory 表），其中 8 字节是 posting 本身。
- **为什么词典用「有序 Vec + 二分」而不是 HashMap？** 查询只有几个词，log(n) 次字符串比较可忽略；有序的好处是第二阶段写 FST 词典、第三阶段前缀查询（prefix query）都需要有序遍历。内存也更省。
- **为什么 `get_mut` 而不是 `entry()`？** `entry()` 需要先拥有 key（每个 token 都要 clone 一个 String），`get_mut` 先查，只有新词才分配。
- 取舍：tf 统计用「排序后分组」，O(n log n)；用 HashMap 计数也行。排序版代码短、没有额外哈希表。

**(c) 语法**
- `std::mem::take(&mut self.token_buf)` —— 把字段里的值「拿出来」，原地留一个空 Vec。用来绕开借用检查器（borrow checker）：同时「借 `self.analyzer`」和「改 buffer」会冲突，拿出来就成了独立变量。用完再放回。
- `u32::try_from(len).expect("too many documents")` —— 带检查的类型转换，返回 `Result`；`expect` 失败就 panic 并带上信息。
- `tokens.chunk_by(|a, b| a == b)` —— 切片方法，把相邻满足条件的元素分成一组组子切片。
- `pub fn build(self) -> InMemorySegment` —— 参数是 `self`（不是 `&self`）：**消耗**（consume）builder，调用后 builder 不能再用。所有权系统在编译期保证「冻结后不能再加文档」。
- `self.postings.into_iter().collect()` —— `into_iter` 拿走所有权逐个产出元素，`collect` 收集成目标容器（类型由左边的标注决定）。
- `impl Iterator<Item = (&str, &[Posting])> + '_` —— 返回「某个实现了 Iterator 的类型」，不用写出具体类型；`'_` 表示借用自 self。
- `.map_or(0, |p| p.len() as u32)` —— Option 上：None 给默认值，Some 用闭包变换。
- `type Postings<'a> = std::iter::Copied<std::slice::Iter<'a, Posting>>;` —— 内存段的 postings 迭代器就是「遍历切片并复制每个元素」。

---

## 6. `crates/strata-core/src/scoring.rs` —— BM25

**(a) 骨架**
```
score(D,Q) = Σ_t  qtf(t) · IDF(t) · tf·(k1+1) / (tf + k1·(1 − b + b·|D|/avgdl))
IDF(t)     = ln(1 + (N − df + 0.5)/(df + 0.5))
```
- `Bm25Params { k1, b, doc_length }`，常量 `ANSERINI`（0.9, 0.4）和 `CLASSIC`（1.2, 0.75）。
- `idf(N, df)`：f64 计算再转 f32。
- `Bm25TermScorer`：每个查询词一个，把与文档无关的部分预先算好（`qtf·IDF·(k1+1)`、`k1·(1−b)`、`k1·b/avgdl`），每个 posting 只剩几次乘加和一次除法。
- `DocLengthEncoding::{Exact, LuceneByte4}` + `lucene_quantize_length`：移植 Lucene `SmallFloat.intToByte4/byte4ToInt`，把文档长度压成 1 字节（< 24 精确，之后保留 4 个有效位，最多低估 12.5%）。

**(b) 为什么**
- **为什么用 `ln(1+…)` 版 IDF？** 原始 Robertson–Spärck Jones IDF `ln((N−df+0.5)/(df+0.5))` 在 df > N/2 时为负——匹配了查询词反而扣分。Lucene 版永远为正。
- **(k1+1) 要不要？** 它对所有分数等比缩放，不影响排序。Lucene 8+ 去掉了，我们和教科书 / Tantivy 一样保留。
- **为什么要模拟 Lucene 的长度量化？** 为了解释和 Anserini 的差距：开启后 3 个数据集四位小数完全一致。同理，开启后和 Tantivy 的排名逐条一致（Tantivy 的 fieldnorm 编码和 Lucene 相同），这是一个强有力的交叉验证。默认仍用精确长度，因为它是「正确」的公式；量化是 Lucene 为省空间做的工程取舍（每文档每字段 1 字节 norm）。
- 预计算是典型的「把循环不变量提出循环」（loop-invariant hoisting），Lucene 更进一步把 256 个可能的 norm 值预先算成表。

**(c) 语法**
- `pub const ANSERINI: Bm25Params = Bm25Params { ... };` —— 关联常量（associated const），`Bm25Params::ANSERINI`。
- `Bm25Params { doc_length, ..self }` —— 字段简写 + 结构体更新。
- `#[inline]` —— 建议编译器内联这个热函数。
- `i.leading_zeros()`、`>>`、`& 0x07` —— 位运算，对应 Java 的 `Long.numberOfLeadingZeros`。
- `u64::from(len)` —— 无损转换（`From` trait），比 `as` 更安全：只有不会丢精度的转换才有 `From`。

---

## 7. `crates/strata-core/src/search.rs` —— 查询与 top-k

**(a) 骨架**
- `Query`：词袋，`Vec<QueryTerm { term, count }>`，同一个词出现两次 count=2（和 Anserini 的 `BagOfWordsQueryGenerator` 一样，用 boost 表示）。
- `top_k(hits, k)`：大小为 k 的**小顶堆**（min-heap）选最好的 k 个，O(n log k)。
- `Searcher<'s, S: SegmentReader>`：`search(query, k)` 用 **TAAT**（term-at-a-time，逐词）穷举打分：
```
scores = [0.0; N];  touched = []
for 每个查询词 t:  for (doc, tf) in postings(t):
    if scores[doc] == 0: touched.push(doc)
    scores[doc] += bm25(...)
return top_k(touched 里的文档)
```

**(b) 为什么**
- **为什么穷举？** 第一阶段要一个「绝对正确」的基线，第三阶段 WAND / Block-Max WAND（动态剪枝 dynamic pruning：跳过不可能进 top-k 的文档）必须和它结果一致。
- **TAAT vs DAAT**（document-at-a-time，逐文档）：TAAT 需要 O(N) 的累加器数组，但实现最简单、顺序访问 postings；DAAT 同时推进所有词的游标，内存 O(查询词数)，是 WAND 的前提。第三阶段换 DAAT。
- **为什么记 `touched`？** 选 top-k 时只扫匹配过的文档，而不是全部 N 篇。BM25 每个贡献都 > 0，所以「分数非 0」⇔「至少匹配一个词」。
- **为什么小顶堆？** 维护「目前最好的 k 个」，堆顶是第 k 好的（门槛 threshold）；新文档只需和堆顶比。全排序是 O(n log n)。这个「门槛」正是 WAND 剪枝要用的值。
- **平分怎么办？** 分数相同时 doc_id 小的优先，保证结果确定（deterministic）。评测时再按 trec_eval 规则重排。
- 取舍：每次查询分配一个 N 长的 f32 数组（trec-covid 是 171k × 4B ≈ 685KB）。可以复用，但需要可变状态或线程局部缓冲；第一阶段选清晰。

**(c) 语法**
- `impl Ord for Ranked` —— `f32` 因为 NaN 不是全序（`Ord`），不能直接放堆里；包一层 newtype 用 `total_cmp` 自定义全序。`then_with` 定义次级比较。
- `BinaryHeap<Reverse<Ranked>>` —— Rust 的 `BinaryHeap` 是大顶堆，`Reverse` 反转比较变成小顶堆。
- `heap.peek_mut()` —— 拿到堆顶的可变引用；通过它写入后，守卫（guard）被丢弃时自动下沉调整（sift-down），比 pop+push 少一次操作。
- `let Some(postings) = self.segment.postings(&qt.term) else { continue; };` —— **let-else**：匹配成功就绑定变量，否则执行 else（必须跳出：continue/return/break）。
- `impl<'s, S: SegmentReader> Searcher<'s, S>` —— 泛型 + trait 约束（trait bound）+ 生命周期：searcher 借用段，借用期间段不能被释放——编译器保证没有悬垂指针（dangling pointer）。
- `impl IntoIterator<Item = String>` 作为参数 —— 接受 Vec、数组、任何迭代器。

---

## 8. `crates/strata-core/tests/search_vs_brute_force.rs` —— 随机化性质测试

**(a) 骨架**：proptest 随机生成小语料（8 个词的词表，最多 40 篇，每篇 ≤30 词）、随机查询、随机 k、随机 k1/b；
用一个**完全不碰倒排表**的朴素实现（f64，直接按公式数 tf、df）算每篇文档分数，然后断言：结果数正确、每个分数一致（容差 1e-4）、降序、没有漏掉比第 k 名更好的文档。

**(b) 为什么**：单元测试只覆盖想得到的例子；性质测试（property-based testing）覆盖想不到的。词表小 → df 高、平分多 → 专门压边界。proptest 失败时会**缩小**（shrink）到最小反例，便于调试。第三阶段的 WAND 也会加进这个测试。

**(c) 语法**：`proptest! { #[test] fn name(docs in corpus(), k in 1usize..50, ...) { prop_assert!(...) } }` —— 宏生成测试，`in` 后面是「生成策略」（Strategy）。`impl Strategy<Value = Vec<Vec<usize>>>` —— 返回一个生成器。

---

## 9. `crates/strata-eval/src/beir.rs` —— BEIR 数据加载

**(a) 骨架**：读 `corpus.jsonl`（`_id/title/text`）、`queries.jsonl`、`qrels/test.tsv`（首行表头）。`Document::flat_text()` = 标题 + 空格 + 正文（Anserini 的「flat」设置）。只保留有标注的查询，按 ID 排序保证可复现。

**(b) 为什么**：BEIR 是零样本（zero-shot）检索的标准基准，18 个数据集；我们只用小/中型的（scifact、nfcorpus、fiqa、scidocs、trec-covid），磁盘有限，明确拒绝 MS MARCO。

**(c) 语法**
- `#[serde(rename = "_id")]`、`#[serde(default)]` —— serde 派生宏的字段属性：JSON 键名映射、缺失时用默认值（有些文档没有 title）。
- `fn load(...) -> Result<Dataset>` + `?` —— **错误处理**：`Result<T, E>` 要么 Ok 要么 Err；`?` 遇到 Err 就提前返回（类似 Go 的 `if err != nil { return err }`，但一个字符）。`anyhow::Result` 是「任意错误」的便捷类型，`.with_context(|| ...)` 给错误加上「哪个文件第几行」。
- `bail!(...)` —— 立即返回一个错误。
- `let [qid, docid, rel] = cols[..] else { bail!(...) };` —— 切片模式 + let-else：正好 3 列才绑定。
- `fn read_jsonl<T: serde::de::DeserializeOwned>(...)` —— 泛型函数，对任何可反序列化类型复用。
- 测试里 `struct TempDir; impl Drop for TempDir` —— `Drop` 是析构函数（RAII），离开作用域自动删临时目录，即使测试 panic。

---

## 10. `crates/strata-eval/src/metrics.rs` —— 评测指标

**(a) 骨架**
- `ndcg_at_k`：`DCG@k = Σ gain/log2(i+1)`，gain = 标注等级（线性，trec_eval 默认，不是 2^rel−1）；除以理想排序的 DCG（理想排序用该查询**所有**标注为正的文档）。
- `recall_at_k`：top-k 里的相关文档数 / 全部相关文档数（rel ≥ 1）。
- `mrr_at_k`：第一个相关文档排名的倒数，k 之外记 0。
- `trec_order`：按分数降序、**分数相同按 doc id 字符串降序**——这是 trec_eval 的规则。
- `evaluate`：对 qrels 里**所有**查询取平均，没有结果的查询记 0（trec_eval `-c`）。

**(b) 为什么**
- 指标实现「差之毫厘」就和论文数字对不上：线性 gain vs 指数 gain、平分怎么排、没结果的查询算不算——每一个都会让第三位小数变化。所以用 pytrec_eval（trec_eval 的 C 实现包装，BEIR 官方用它）**交叉验证**：5 个数据集、均值和每个查询的值都完全一致（差 ≤ 4e-15，见 `results/crosscheck/`）。
- `per_query` 用 `BTreeMap`：有序，JSON 输出稳定，git diff 干净。

**(c) 语法**
- `pub type Qrels = HashMap<String, HashMap<String, i32>>;` —— 类型别名。
- `judgments.get(doc).copied().unwrap_or(0).max(0)` —— 链式 Option 处理：取值 → 复制出来 → 缺省 0 → 负数截成 0。
- `.is_some_and(|&rel| rel > 0)` —— Option 上的谓词；`|&rel|` 在闭包参数里解引用。
- `.position(...)` —— 返回第一个满足条件的下标（`Option<usize>`）。
- `results.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.cmp(&a.0)))` —— 自定义比较器；交换 a/b 就是降序。

---

## 11. `crates/strata-eval/src/harness.rs` —— 实验工具

**(a) 骨架**：TREC run 文件读写（`qid Q0 docid rank score tag`）、`LatencyStats`（p50/p90/p99/max/QPS）、`percentile`（nearest-rank 最近秩法）、`mean_std`（样本标准差，n−1）。

**(b) 为什么**
- 分数用 Rust 的「最短可往返」浮点格式输出：读回来是完全相同的 f64。因为 trec_eval 按分数排序，四舍五入可能改变平分关系、改变指标。
- 百分位数定义要写清楚（nearest-rank），否则 p99 在 50 个查询的数据集（trec-covid）上含义模糊：50 个样本时 p99 就是最大值。这一点在 BENCHMARK.md 里注明了。

**(c) 语法**：`BufWriter` / `BufReader` 缓冲 I/O；`writeln!(out, ...)?` 写入并传播错误；`let total: Duration = sorted.iter().sum();` —— `Sum` trait 让 `Duration` 也能 sum。

---

## 12. `crates/strata-eval/src/main.rs` —— `strata-eval` 命令行

**(a) 骨架**：`run` 子命令：加载 → 每次重复（repeat）：纯分析计时 → 建索引计时 → 跑全部查询（k=1000，计时每个查询）→ 再跑一遍 k=10 计时 → 断言每次重复结果完全相同 → 写 run 文件 + JSON 报告（含每次重复的数据、均值±标准差）。`eval` 子命令：给任意 run 文件打分。

**(b) 为什么**
- **计时口径**：查询延迟只算「查询分析 + 检索」，不含把结果转成字符串；单线程；同一进程重复 3 次报告均值和标准差。
- **为什么另跑 k=10？** k=1000 是评测要求（Recall@1000），k=10 是真实「第一页」场景，也是第三阶段 WAND 最能体现价值的场景。
- `--keep-self-hits`：默认像 Anserini 的 `-removeQuery` 一样，去掉 doc id 等于 query id 的结果（BEIR 里某些数据集查询本身也在语料里）。
- 断言确定性（determinism）：同样输入每次输出必须完全一致，否则说明有隐藏状态 / 并发 bug。

**(c) 语法**
- `#[derive(Parser)]`、`#[arg(long, default_value_t = 0.9)]`、`#[command(subcommand)]` —— clap 的派生宏，从结构体定义自动生成命令行解析和 `--help`。
- `#[derive(ValueEnum)] enum DocLen { Exact, Lucene }` —— 命令行里写 `--doc-length lucene`。
- `fn main() -> Result<()>` —— main 也可以返回 Result，出错时打印错误链并以非 0 退出。
- `let col = |f: fn(&Repeat) -> f64| repeats.iter().map(f).collect::<Vec<f64>>();` —— 闭包接收函数指针；`collect::<Vec<f64>>()` 是 turbofish 语法，显式指定收集的类型。

---

## 13. `crates/strata-eval/src/bin/strata-mem.rs` —— 内存测量

**(a) 骨架**：用 `stats_alloc` 替换全局分配器（global allocator），统计分配/释放字节；在「加完所有文档」和「build 完成」两个点读「存活字节 = 分配 − 释放」。

**(b) 为什么**
- `heap_size_bytes()` 是按容量（capacity）算的估计值；分配器统计是「真值」。两者在 5 个数据集上**完全相等**，说明估算函数没有漏算的结构。
- 单独做一个二进制：计数用原子操作（atomic），会拖慢程序，不能污染计时实验。
- 没包含系统分配器自身的元数据/对齐浪费，文档里注明。

**(c) 语法**：`#[global_allocator] static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;` —— 整个程序的堆分配都经过它。我们的代码没有 `unsafe`（unsafe 在依赖库内部）。

---

## 14. `crates/strata-bench/` —— Tantivy 对照组

**(a) 骨架**
- `lib.rs`：`StrataTokenizer` 实现 Tantivy 的 `Tokenizer` trait，把 strata 的 `Analyzer` 塞进 Tantivy → 两个引擎索引**完全相同的词**。
- `main.rs`：1 个索引线程、内存索引、合并成 1 个段；查询 = 每个不同的词一个 `TermQuery`，重复词用 `BoostQuery`，`Should`（OR）组合；`TopDocs::with_limit(k).order_by_score()`（内部走 Block-WAND）；用同一套指标代码评测。`--tokenizer native` 用 Tantivy 自带分析链（`SimpleTokenizer` + 小写 + 同样停用词 + Porter2）。

**(b) 为什么**
- 变量控制：同样的词、同样 BM25 参数（Tantivy 写死 k1=1.2, b=0.75）、同样长度量化 → 排名应该一致。实测 top-10 列表在 scifact 上 300/300 完全一致，分数差 ~1e-5（f32 运算顺序不同）。这说明我们的 BM25 实现和一个成熟引擎逐条吻合。
- 性能对比要诚实：Tantivy 的索引是**压缩的**（scifact 约 1.2MB vs 我们未压缩 5.5MB），还存了 fast field 等；它的查询有每次构建 Weight、FST 查词、解压 block 的固定开销。小语料上我们的穷举反而更快，但这**不是**「我们比 Tantivy 快」的证据——数据规模一大、或者第二阶段我们也压缩了，结论可能反转。（BENCHMARK.md 里有完整表格和这些限定。）

**(c) 语法**：`type TokenStream<'a> = StrataTokenStream;` —— 实现别人定义的 GAT；`Arc<Analyzer>` —— 原子引用计数智能指针，多个 clone 共享一个分析器（Tantivy 要求 Tokenizer: Clone + Send + Sync）；`Box<dyn Query>` —— trait 对象（动态分发），这里用是因为 Tantivy 的 API 就是这么设计的。

---

## 15. `scripts/`

- `download_beir.sh`：从 BEIR 官方镜像下载解压到 `data/`（gitignored），拒绝 msmarco。
- `crosscheck_pytrec.py`：`uv run --with pytrec_eval==0.5 --with numpy` 临时环境，比较均值和逐查询值，容差 5e-5。
- `compare_runs.py`：两个 run 文件逐查询比较 top-k 列表/集合/分数差。
- `run_all.sh`：一键复现 BENCHMARK.md 所有数字（环境信息、配置矩阵、交叉验证、排名一致性、内存）。

---

# 面试问答（基于代码）

**Q1. BM25 公式每一项的含义？怎么来的？**
BM25 来自概率检索模型（probabilistic relevance framework / binary independence model）。三部分：
- IDF：词越稀有越有区分度。Robertson–Spärck Jones 权重 `log((N−df+0.5)/(df+0.5))` 来自「相关文档里出现该词的概率 vs 不相关文档里的概率」的对数几率比，在没有相关性信息时的近似（+0.5 是平滑）。
- TF 饱和：`tf·(k1+1)/(tf+K)`，tf→∞ 时趋于 k1+1。出现 1 次和 2 次差别大，100 次和 101 次几乎没差别（来自 2-Poisson 「eliteness」模型的近似）。
- 长度归一化：`K = k1·(1−b+b·|D|/avgdl)`：长文档天然 tf 大，要惩罚。
代码：`scoring.rs` 的 `Bm25TermScorer::score`；`tf_saturates_towards_idf_times_k1_plus_1` 测试验证了饱和性质。

**Q2. IDF 有哪些变体？为什么选 Lucene 那个？**
经典 RSJ `ln((N−df+0.5)/(df+0.5))`：df > N/2 时为负；ATIRE/BM25+ 等有各自修正；Lucene `ln(1 + (N−df+0.5)/(df+0.5))` 恒正。选 Lucene 版：(1) 不会出现「匹配反而扣分」；(2) 和 Anserini/Tantivy 一致才能对数字。测试 `idf_hand_computed` 里 df=N 时仍为正。

**Q3. k1 和 b 怎么理解？为什么 Anserini 用 0.9/0.4？**
k1 控制 tf 饱和速度（0 = 只看是否出现；大 = 接近线性），b 控制长度归一化强度（0 = 不归一；1 = 完全按 |D|/avgdl）。1.2/0.75 是 Robertson 等人的经典默认；0.9/0.4 是 Anserini 在 MS MARCO 上调出来的，BEIR 沿用。我们两组都测了：在这 5 个数据集上 1.2/0.75 反而全部更好（nDCG@10 +0.0010 到 +0.0196，见 BENCHMARK.md 的消融表）——说明在一个数据集（MS MARCO）上调出来的参数不一定迁移（transfer），复现论文要用论文参数，做产品要按自己的数据调。

**Q4. 倒排索引的内存布局？为什么这样设计？**
CSR：有序词典 `Vec<String>` + `term_offsets` + 一个扁平的 `Vec<Posting>`，加 `doc_lens: Vec<u32>`。理由见第 5 节：少分配、缓存友好、和磁盘格式同构、有序支持 FST/前缀。实测 9.3–10.7 字节/posting（8 字节 posting + 词典 + 偏移 + 文档 ID 字符串分摊）。第二阶段用差值编码 + 变长整数（varint）/位打包（bitpacking）压缩；作为参照，Tantivy 整个索引折合约 1.8–2.4 字节/posting（scifact 1.2MB vs 我们 5.5MB，小 4.5–5.1 倍）。

**Q5. nDCG 的定义？为什么要「归一化」？graded relevance 怎么处理？**
DCG@k = Σ_{i=1..k} gain_i / log2(i+1)；IDCG 是理想排序的 DCG；nDCG = DCG/IDCG ∈ [0,1]，不同查询（相关文档数不同）之间可比。gain 用标注等级本身（trec_eval 线性 gain），所以 rel=2 的文档贡献是 rel=1 的两倍。理想排序用该查询所有标注为正的文档（不只是检索到的）。手算例子在 `metrics.rs` 的 `ndcg_hand_computed`。

**Q6. 你怎么确定自己的指标实现是对的？**
三层：手算单元测试；trec_eval 细节（平分按 doc id 降序、`-c` 语义）；用 pytrec_eval 在同一个 run 文件上交叉验证，5 个数据集均值和逐查询值都一致（差 ≤ 4e-15）。另外，和 Anserini 发表数字四位小数一致，也间接验证了整条链路。

**Q7. 你的 BM25 和 Anserini 差在哪？怎么定位的？**
逐个排除：(1) 分词：比较总词数（scifact 838,125 vs 838,128）；(2) 词干器：rust-stemmers 是 Porter2，Lucene 是 Porter → 自己移植 Porter，用官方 23,531 词验证，换上后差距大幅缩小；(3) 文档长度：Lucene 把长度压成 1 字节 → 移植 SmallFloat 做成开关，开启后 scifact/fiqa/scidocs 三项指标四位小数一致；(4) 剩下 nfcorpus、trec-covid 的 nDCG@10 差 ~0.001：总词数差 0.07% 和 0.01%，推测是分词边界的边角情况（比如 Lucene StandardTokenizer 会为 ©/® 这类 emoji 字符产生 token，Unicode 版本不同），**没有运行 Lucene 逐 token 对比，所以这只是推测**。

**Q8. top-k 为什么用堆？复杂度？**
n 个候选选 k 个：小顶堆 O(n log k)、O(k) 内存；全排序 O(n log n)；quickselect 平均 O(n) 但要 O(n) 内存且不保序。k ≪ n 时堆最好，而且堆顶就是 WAND 需要的「当前门槛分」。

**Q9. TAAT 和 DAAT 有什么区别？WAND 是什么？**
TAAT 逐词遍历 postings、累加到数组，简单，但必须扫完所有 postings、要 O(N) 累加器。DAAT 所有词的游标按 doc id 同步前进，一次算完一篇文档。WAND（Weak AND）利用每个词的分数上界：如果若干词上界之和都达不到当前第 k 名分数，就可以直接跳过这些文档；Block-Max WAND 把上界细化到每个 posting block（比如 128 个文档一块），剪枝更狠。第三阶段做，用这里的穷举结果做正确性对照（proptest）。

**Q10. 为什么 Tantivy 在小数据集上查询比你慢？这能说明你更快吗？**
不能。我们的 postings 是未压缩的 `u32`，直接顺序读；Tantivy 要解码 bitpacked block、每次查询构建 Weight（查 FST 词典、算 BM25 统计）、走通用的 collector 框架。小语料上固定开销占主导。我们的索引大 ~4.5 倍，功能少得多（无位置、无删除、无持久化）。第二阶段压缩后要重新测，诚实对比。另外我没有对 Tantivy 做 profile，上面的原因是推测。

**Q11. 为什么在 Tantivy 里用你自己的分词器？**
控制变量：让比较只关于「索引 + 打分」，不被分词差异干扰。结果两边 top-10 逐条一致，证明 BM25 实现等价；`native` 分词器是另一组对照（Tantivy 的 SimpleTokenizer 只按非字母数字切，`3.14` 会被切开）。

**Q12. 为什么要把 SegmentReader 做成 trait？GAT 的代价是什么？**
为了第二阶段换存储不改搜索代码。用关联类型（GAT）而不是 `Box<dyn Iterator>`：静态分发、可内联，热循环快；代价是 trait 不能做成 `dyn SegmentReader`（object safety），多段搜索要用泛型或 enum 包装。这是有意识的取舍。

**Q13. 全局统计量（global IDF）问题是什么？你怎么为它留口子？**
IDF 和 avgdl 依赖整个集合。如果每个段/分片用自己的统计量，同一篇文档在不同分片得分不同，scatter-gather 合并 top-k 时排序就错了。`Searcher::with_stats` 允许传入全局 `CollectionStats`（测试 `external_stats_change_scores`）。目前 df 还是段内的——第二阶段多段时需要一个全局词统计源，这是已知的缺口。

**Q14. 你怎么保证搜索结果是对的，而不只是「看起来合理」？**
(1) 手算 BM25 例子；(2) proptest 随机语料对比朴素暴力实现（不碰倒排表）；(3) 和 Tantivy 排名逐条一致；(4) 和 Anserini 发表数字四位小数一致；(5) 每次重复的 run 完全相同（确定性断言）。

**Q15. 性能数字是怎么测的？可信吗？**
单机（Apple M2，8 核，24GB，macOS），release 构建，单线程索引和查询，进程内重复 3 次报告均值±标准差，原始输出在 `results/`。局限：本机测试（没有网络开销）、笔记本有睿频/大小核调度/后台进程噪声、p99 在 50 个查询的数据集上就是最大值。所有数字都能用 `scripts/run_all.sh` 复现。

**Q16. 索引速度的瓶颈在哪？怎么优化？**
实测分析（分词+小写+停用词+词干）占索引时间的 70–77%（JSON 报告里 `analyze_secs` vs `index_secs`，例如 trec-covid 6.08s / 8.35s）。优化方向：rayon 多线程并行分析（文档之间独立）；减少每个 token 的 String 分配（arena / 复用缓冲区 / 先查词典再分配）；每个线程建自己的段再合并（正好是第二阶段的段架构）。

**Q17. Rust 的所有权在这个项目里具体帮了什么？**
`build(self)` 消耗 builder：编译期保证冻结后不能再加文档；`Searcher<'s, S>` 借用段：编译期保证段不会在搜索时被释放；不可变段可以放心多线程共享（第二阶段的快照隔离读者 snapshot-isolated readers）。这些在 Java/Go 里要靠约定或运行时检查。
