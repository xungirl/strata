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

---

# Stage 2：存储引擎（Storage engine）

> 新 crate：`crates/strata-storage`。目标：把第一阶段的内存索引变成**持久化、可崩溃恢复、可并发读**的索引，同时保证结果和第一阶段**逐字节一致**。
> 教学顺序：`codec.rs → segment.rs → buffer.rs → livedocs.rs → wal.rs → manifest.rs → merge.rs → index.rs → snapshot.rs → bin/strata-chaos.rs`

## 全局图

```
写路径： add_documents ──► WAL append + fsync（= ack）──► 内存 WriteBuffer + tombstones
                                                     │ commit()
                                                     ▼
          flush 成新 segment ─► 写 tombstone 文件 ─► 原子替换 manifest ─► 删除旧 WAL ─► 发布新 Snapshot
                                                     │ 后台线程
                                                     ▼
                                      相邻同级 segment 合并（丢掉已删除文档）
读路径： index.snapshot() ──► Arc<Snapshot>（不可变的 segment 列表 + tombstones）──► 全局统计量打分
恢复：   open() ─► 读 manifest ─► 打开 segment ─► 清理孤儿文件 ─► 重放 committed_seq 之后的 WAL
```

## 1. `codec.rs` —— 整数压缩

**(a) 骨架**：`bits_needed`、`pack/unpack`（定宽位打包 bit packing）、`write_varint/read_varint`（LEB128 变长整数）、小端读取辅助函数。
**(b) 为什么**：倒排表存的是**差值**（delta / gap：`[3,7,8,20] → [3,4,1,12]`），差值小，所以用最大值所需的位数打包，常常 4–8 位就够，而不是 32 位。位置信息（positions）数值分布很散，用 varint。这是 Lucene（PackedInts + VInt）、Tantivy（128 一块的 bitpacking）的经典做法。我们的实现是标量（一次一个值），没做 SIMD 解码（这是以后的优化点）。
**(c) 语法**：`u64` 当作位缓冲区 `acc`，`acc |= v << acc_bits` 往上塞，`out.push(acc as u8)` 从下面取字节；`div_ceil` 向上取整；`u32::from_le_bytes(slice.try_into().expect(..))` —— `try_into` 把 `&[u8]` 转成定长数组 `[u8; 4]`，长度不对就返回错误。测试里用了 proptest 做往返（roundtrip）测试。

## 2. `segment.rs` —— 不可变的磁盘段

**(a) 骨架**：一个 segment 是一个目录，写一次永不修改：
```
terms.fst      词 → postings.bin 里的字节偏移（FST，有限状态转换器）
postings.bin   每个词：头(df, positions 偏移, 块数) + 跳表(skip table) + 128 文档一块的位打包块
positions.bin  每个 posting 的位置（varint 差值）
doclens.bin    每篇文档长度 u32
ids.fst/ids.bin  外部 id ↔ 文档号
meta.json      格式版本、计数、分析器配置
```
- `SegmentWriter`：按词典序 `add_term`，最后 `finish` 写文档级文件并 fsync 所有文件和目录。
- `DiskSegment`：mmap 打开，实现第一阶段的 `SegmentReader` trait。
- `DiskPostings`：块解码游标；`next`、`advance_to(target)`（用跳表二分跳块）、`positions()`、`block_for(target)`（不解码，只看跳表的「浅前进」shallow advance）。

**(b) 为什么**
- **为什么不可变？** 写一次、只读多次：并发读不需要锁；崩溃时不会有「改了一半」的文件；缓存友好。代价是删除只能打标记（tombstone），更新 = 删除 + 新增，靠合并回收空间。这就是 Lucene/Tantivy 的 segment 架构，也是 LSM-tree 的思想。
- **为什么 FST？** 词典是有序字符串集合，FST 共享前缀**和后缀**，比哈希表小得多，还支持前缀/范围遍历（第三阶段的 prefix query 就靠它）。值存的是 postings 偏移（递增），FST 压缩得也很好。
- **跳表里存 `max_tf` 和 `min_doc_len`**：BM25 随 tf 增大、随文档长度增大而减小，所以 `score(max_tf, min_doc_len)` 是这个块里任何文档该词得分的**上界**，而且和集合统计量无关（合并后依然成立）。这是给第三阶段 block-max WAND 准备的。
- **单块的词不写跳表**：大部分词很稀有（只有一个块），省掉 20 字节跳表项。这个优化是看了文件大小才做的：最初 scifact 的 postings.bin 里一半是跳表/词信息开销。改完后 advance_to 的 proptest 立刻抓到一个 bug（单块词的 last_doc 未知，扫描必须设上界）。
- **mmap 的取舍**：由操作系统按需分页、自动缓存，冷段不占内存；`unsafe` 只有一处（`map_file`），SAFETY 注释写明了前提：段文件写完 fsync 后永不修改或截断。代价：I/O 错误会变成 SIGBUS 而不是 `Result`；也没法控制缓存淘汰。

**(c) 语法**
- `pub enum Bytes { Mapped(Mmap), Owned(Vec<u8>) }` + `impl Deref for Bytes` —— 两种来源统一成 `&[u8]` 用（空文件在有的系统上不能 mmap）。
- `unsafe { Mmap::map(&file) }` —— `unsafe` 块：编译器无法验证的前提由程序员保证，配一段 `// SAFETY:` 注释是 Rust 社区的惯例。
- `impl Drop for DiskSegment` —— 析构：如果被标记 `obsolete`（被合并掉了），最后一个引用释放时删除目录。`AtomicBool` 保证跨线程可见。
- `struct DiskPostings<'a> { blocks: &'a [u8], ... }` —— 游标借用 mmap 的字节，生命周期 `'a` 保证游标不能比 segment 活得久。
- `impl Iterator for DiskPostings<'_>` —— 实现标准迭代器，所以第一阶段的泛型 `Searcher` 能直接用在磁盘段上（零改动复用）。

## 3. `buffer.rs` —— 写缓冲

**(a) 骨架**：`WriteBuffer`：`HashMap<term, Vec<PostingWithPositions>>`、doc_lens、ids、`by_id`（id → 缓冲区内最新的文档）、`deleted`。`flush` 排序后交给 `SegmentWriter`，返回本段里需要立刻打 tombstone 的文档号。
**(b) 为什么**：类似 Lucene 的 RAM buffer。位置（position）是去停用词**之后**的下标（没有空位），和 Lucene 保留空位不同——文档写明了这个差异。缓冲区里被删除/被替换的文档仍然写进段里再打标记，这样段内文档号严格等于插入顺序，排序的平局规则（tie-break）和第一阶段一致。
**(c) 语法**：`by_term.chunk_by(|a, b| a.0 == b.0)` 分组；`if let Some(old) = self.by_id.insert(..)` —— `HashMap::insert` 返回旧值，正好用来实现 upsert。

## 4. `livedocs.rs` —— 删除标记（tombstones）

**(a) 骨架**：每个段一个位图（1 = 已删除），带 CRC32 校验的文件格式。
**(b) 为什么**：段不可变，删除只能记在旁边。和 Lucene 一样，已删除但未合并的文档**仍然计入 N 和 df**（打分统计量），合并后才消失——这一点在测试里专门验证了（不合并 vs 全部合并两种 oracle）。
**(c) 语法**：`#[derive(Clone, PartialEq, Eq)]`；`Arc::make_mut(&mut s.live).delete(doc)`（在 index.rs 里）—— **写时复制**（copy-on-write）：如果这个 `Arc` 只有我一个持有者就原地改，否则先克隆一份再改，旧快照手里的那份不受影响。快照隔离就靠这一行。

## 5. `wal.rs` —— 预写日志（write-ahead log）

**(a) 骨架**：记录格式 `len | crc32 | payload(seq, op, id, text)`；`Wal::append(first_seq, ops)` 写一批再 `sync_data` 一次；`replay(dir, committed_seq)` 返回需要重放的操作；`remove_obsolete`。
**(b) 为什么**
- **先写日志再改内存，fsync 返回后才 ack**：崩溃后能丢的只有没 ack 的东西。
- **组提交（group commit）**：一次 fsync 覆盖一批记录，fsync 是最贵的操作（BENCHMARK 里有 batch=1/10/100/1000 的吞吐对比）。
- **撕裂写（torn write）**：崩溃可能留下半条记录。只有**最后一个文件的尾部**可能撕裂（只有它在被追加），所以那里长度不够/CRC 不对就截断；更早文件里的 CRC 错误不可能是撕裂写，报错而不是静默丢数据。
- 序号必须连续（`seq gap` 报错），防止中间丢记录。
**(c) 语法**：`OpenOptions::new().create(true).append(true).open(..)`；`file.sync_data()`（只刷数据和必要的元数据）；`File::open(dir)?.sync_all()` —— 对**目录**做 fsync，让新文件的目录项也持久化（POSIX 上新建文件后必须这样做，很多实现会漏）。

## 6. `manifest.rs` —— 提交点

**(a) 骨架**：`manifest.json` 记录 generation、`committed_seq`、段列表（id、文档数、tombstone 代数）。`store`：写临时文件 → fsync → `rename` → fsync 目录。
**(b) 为什么**：`rename` 在 POSIX 上是原子的，崩溃后 manifest 要么是旧的要么是新的，不会半新半旧。manifest 没引用的文件都是垃圾（崩溃时写了一半的段），打开时清理。`committed_seq` 把 manifest 和 WAL 连起来：≤ 它的操作已经在段里了，> 它的要重放。

## 7. `merge.rs` —— 段合并

**(a) 骨架**：`merge_segments(sources, ..)`：先给存活文档重新编号（逐段、按顺序），再用 FST 的 `OpBuilder::union()` 按词典序流式合并所有段的词典，每个词把各段的 postings 拼起来（文档号重映射，跳过已删除），写出新段。
**(b) 为什么**
- 段太多 → 每次查询要查很多个词典；删除的文档永远占空间。合并 = LSM 的 compaction。
- **只合并相邻的段**（类似 Lucene 的 LogMergePolicy，按大小分层 tier = ⌊log_factor(存活文档数)⌋）：保持存活文档的相对顺序不变，所以平局规则、因而结果，和不合并完全一样。TieredMergePolicy 允许不相邻合并，能更好地选「最划算」的合并，但会改变文档顺序。
- 合并期间不持有写锁（写入、提交可以继续）；合并完成时在锁内把「合并期间新发生的删除」搬到新段上（对比开始时的 tombstone 快照和现在的）。
**(c) 语法**：`fst::Streamer` 是「借出型迭代器」（lending iterator）：每个元素借用流本身，所以只能 `while let Some(..) = stream.next()`，不能用 `for`。

## 8. `index.rs` —— 写入器、提交、恢复、合并调度

**(a) 骨架**：`Index { state: Mutex<WriterState>, snapshot: RwLock<Arc<Snapshot>>, merge_lock, merge_thread }`。
- `open`：建目录 → 读/建 manifest → 打开段和 tombstone → 清理孤儿 → 重放 WAL。
- `add_documents / delete`：在锁内 WAL append+fsync，然后应用到缓冲区和 tombstone（upsert = 先在所有地方删掉同 id 再加）。
- `commit`：flush → 写脏 tombstone → 写 manifest → 新 WAL 文件、删旧的 → 发布快照 → 触发合并。
- `merge_once`：锁内挑选 → 锁外合并 → 锁内替换并搬运删除。
**(b) 为什么**
- **快照 = `Arc`**：读者拿到 `Arc<Snapshot>` 后，写者再怎么提交/合并都不影响它（快照隔离 snapshot isolation，读不加锁）。被合并掉的段在**最后一个快照释放时**才删文件（引用计数）。测试 `snapshots_are_isolated_from_later_writes_and_merges` 验证了：旧快照仍能看到 4 个段、被删的文档，目录还在；drop 之后目录消失。
- **一个 bug 的故事**：最初提交时会「清理 manifest 没引用的段目录」，结果把旧快照还在用的段（以及正在合并的输出）删了。修复：运行期间段目录只由 `Drop` 删除，孤儿清理只在 `open` 时做（那时不可能有快照）。
- **一个语义 bug**：`force_merge` 只有一个段时什么都不做，导致这个段里删除的文档还在统计量里。修复：单段有删除时也重写（expunge deletes）。
- 限制：没有文件锁防止两个进程同时写同一个目录（文档里写明了）。

**(c) 语法**
- `pub fn commit(self: &Arc<Self>)` —— 方法接收者可以是 `self: &Arc<Self>`，这样方法里能 `Arc::clone(self)` 交给后台线程。
- `std::thread::spawn(move || loop { .. })` —— `move` 闭包把 `Arc<Index>` 的所有权移进线程。
- `self.state.lock().unwrap()` —— `Mutex::lock` 返回 `Result`（如果持锁线程 panic 了锁会「中毒」poisoned）；返回的守卫（guard）离开作用域自动解锁（RAII）。我们刻意用 `{ ... }` 块控制锁的范围，避免持锁调用 `commit`。
- `st.segments.splice(range, [new])` —— 把一段元素替换成新元素，返回被替换的。

## 9. `snapshot.rs` —— 多段搜索

**(a) 骨架**：`Snapshot { segments: Vec<SnapshotSegment{segment, live, base}>, stats }`；`search` / `search_with`：每段用**全局** N、avgdl、df 打分（第一阶段留的 `with_stats` / `with_global_df` 口子），段内结果换成全局文档号（`base + doc`）再统一 top-k。
**(b) 为什么**：多段时如果每段用自己的 IDF，同一篇文档放在不同段里分数不同，排序就错了。用全局统计量 + 全局文档号做平局规则 → 和一个大段完全一样。BEIR 上验证：磁盘引擎（多段、合并后单段）的 run 文件和第一阶段**逐字节相同**。

## 10. `bin/strata-chaos.rs` —— kill -9 混沌测试

**(a) 骨架**：父进程循环：启动子进程（打开索引=恢复，然后不停写：批量新增、偶尔删除、定期提交、后台合并；每次 fsync 成功后在 stdout 打印 `ACK`）→ 随机延迟后 `SIGKILL` → 父进程自己打开索引恢复 → 检查每个被 ack 的新增都能找到、每个被 ack 的删除都找不到。
**(b) 为什么**
- 只在 fsync 之后 ack，所以**丢失的 ack 数必须是 0**。
- 删除操作先打印 `TRY DEL`：如果在 fsync 之后、ack 之前被杀，删除可能生效也可能没生效，两种都对，所以这个 id 不再参与检查——否则测试会误报。
- **负对照（negative control）**：`--broken-ack-before-write` 让子进程先 ack 再写。测试必须报出丢失——证明这个测试**有能力**发现 bug，而不是「永远绿」。
- 局限（写在文档里）：kill -9 只模拟进程崩溃，操作系统页缓存还在，所以**不覆盖断电**；也不覆盖磁盘满、位翻转。
**(c) 语法**：`Command::new(exe).args([...]).stdout(Stdio::piped()).spawn()`；`child.kill()` 在 Unix 上发 SIGKILL；读 stdout 放在单独线程，`BufReader::lines().map_while(Result::ok)`。

## Stage 2 面试问答

**Q1. 为什么段要设计成不可变的？代价是什么？**
不可变 → 读无锁、崩溃不会留下改了一半的文件、可以 mmap 和放心缓存、合并可以后台进行。代价：删除只能打 tombstone；更新 = 删除+新增；需要合并回收空间、控制段数（写放大 write amplification）。

**Q2. 一次写入从调用到可见经历了什么？什么时候算持久化？**
`add_documents` → 编码 WAL 记录 → `write` + `sync_data`（**此刻持久化，返回即 ack**）→ 应用到内存缓冲。**可见**要等 `commit`（或自动 flush）：写段、写 tombstone、原子替换 manifest、发布新快照。持久化和可见是两回事（Lucene 里也是 commit vs refresh）。

**Q3. 崩溃恢复怎么做？怎么保证不重复也不遗漏？**
manifest 记录 `committed_seq`；打开时只重放 WAL 里 seq 更大的记录。新增是 upsert（先删同 id）、删除是幂等的，所以重放一次和执行一次效果一样。提交的原子点是 manifest 的 rename：之前崩溃 → 用旧 manifest + 完整 WAL；之后崩溃 → 新 manifest + 剩下的 WAL。

**Q4. 什么是撕裂写？你怎么处理？**
一次 `write` 只写进去一部分就崩溃，日志尾部留下半条记录。用长度 + CRC 判断，只允许最后一个文件的尾部这样，截断即可——那条记录从没被 ack 过。中间文件的 CRC 错误当作损坏报错。

**Q5. 为什么要 fsync 目录？**
新建文件（新段、新 WAL、rename 后的 manifest）的**目录项**也要持久化，否则崩溃后文件内容在，但目录里找不到它。POSIX 上要对目录 fd 调用 fsync。

**Q6. 快照隔离是怎么实现的？旧快照引用的段被合并了怎么办？**
快照是 `Arc<Snapshot>`，里面是 `Arc<DiskSegment>` 和 `Arc<LiveDocs>`。删除用 `Arc::make_mut` 写时复制，新快照才看到。被合并的段只标记 `obsolete`，最后一个 `Arc` 释放时 `Drop` 删目录；Unix 上即使文件被 unlink，已 mmap 的页仍然有效。

**Q7. 多段打分为什么必须用全局统计量？删除的文档算不算？**
IDF、avgdl 依赖整个集合；按段算会让同一文档得分随所在段变化。我们和 Lucene 一样把未合并的删除文档算进 N 和 df（准确去除需要扫描，代价大），合并后才消失。测试里两种情况都对照了 oracle。

**Q8. 合并策略是什么？为什么只合并相邻段？**
按存活文档数分层（tier = ⌊log_4 n⌋），相邻 4 个同层就合并（类似 LogMergePolicy）。相邻 → 存活文档顺序不变 → 平局规则不变 → 结果和不合并时逐字节一致。代价：没有 TieredMergePolicy 那么灵活（比如优先合并删除比例高的段）。

**Q9. 你的混沌测试证明了什么，没证明什么？**
证明：在真实 SIGKILL 下（包括恰好在刷段、合并、写 WAL 时），所有 ack 过的写入和删除都没有丢、删除的没有复活（具体次数见 BENCHMARK）。负对照证明测试能抓到「先 ack 后写」的 bug。没证明：断电/内核崩溃（页缓存丢失）、磁盘满、静默数据损坏。要测断电需要虚拟机或 dm-flakey 这类故障注入。

**Q10. 压缩后的索引为什么查询比内存版慢？**
磁盘段要做位解包 + 差值还原（每块 128 个），内存版直接读 8 字节的 `Posting`。BENCHMARK 里磁盘引擎 TAAT 的延迟比内存版高（同一次运行里测的），换来的是索引体积小很多、可以持久化、不需要启动时重建。第三阶段的剪枝（MaxScore/BMW）就是为了少解码、少打分。

---

# Stage 3：查询引擎（Query engine）

> 新 crate：`crates/strata-query`；存储层新增 `fields.rs`（文档元数据列）。目标：动态剪枝（dynamic pruning）的 top-k、布尔/短语/前缀查询、元数据过滤、查询语法解析器——并且剪枝结果和穷举**逐位相同**。
> 教学顺序：`strata-storage/src/fields.rs → ast.rs → parser.rs → filter.rs → wand.rs → exec.rs → lib.rs → bin/strata-qbench.rs → tests/query_tests.rs`

## 全局图

```
"+vaccine -mrna \"spike protein\" covi* year:[2019 TO *]"
        │ parser.rs（用索引的分析器分析词和短语）
        ▼
BoolQuery { must, should, must_not, filters }
        │ lib.rs::search
        ├── 纯词的析取（BEIR 查询就是这种）──► MaxScore（默认）/ Block-Max WAND / TAAT 穷举
        └── 其它 ──────────────────────────► exec.rs：DAAT 布尔执行器（交集 / 并集 / 排除 / 短语 / 前缀）
        每段：filter.rs 把过滤条件编译成「这个文档能不能返回」的判断（tombstone + 元数据列）
```

## 1. `strata-storage/src/fields.rs` —— 文档元数据列（doc values）

**(a) 骨架**：`FieldValue::{Keyword, Int}`；每段 `fields.json`（字段名、类型、偏移、关键词字典）+ `fields.bin`（每文档一个定宽槽：int 是 i64，keyword 是字典序号 u32）。`Column::int(doc)` / `ord(doc)` 是数组下标访问。
**(b) 为什么**：过滤要对**候选文档**逐个取值，列式存储（每字段一列）让这一步是 O(1) 数组访问；倒排索引反过来适合「哪些文档有这个值」。Lucene 叫 DocValues。关键词字典放 JSON 里，只适合低基数字段（语言、类别），高基数要用 FST（写明了）。字段类型在 WAL 之前校验，因为一旦 ack 了就不能在 flush 时才失败。
**(c) 语法**：`#[serde(rename_all = "lowercase")]`；`BTreeMap`/`BTreeSet` 保证字典有序；`(v != MISSING_INT).then_some(v)` —— `bool::then_some` 把条件变成 `Option`。

## 2. `ast.rs` + `parser.rs` —— 查询语法

**(a) 骨架**：`Clause::{Term{term,count}, Phrase, Prefix}`、`Filter::{KeywordEq, IntRange, Not}`、`BoolQuery`。解析器：先 `lex` 切分（引号和方括号内不切），再按前缀 `+/-`、引号、`field:`、结尾 `*` 分类。普通词和短语用索引的分析器分析；前缀只小写不做词干（和 Lucene 通配符一样）。`as_disjunction()` 判断能不能走剪枝算法。
**(b) 为什么**：语义照搬 Lucene BooleanQuery（must/should/must_not，没有 coord 因子）；重复的普通词计数（qtf），这样 BEIR 原始查询走解析器也得到和第一阶段一样的 BM25。`12:30` 这种不是合法字段名，不当作过滤。没做括号和 AND/OR（写明是限制）。
**(c) 语法**：`body.strip_prefix('"').and_then(|b| b.strip_suffix('"'))` —— Option 链；`split_once(':').filter(|(f, _)| is_field_name(f))`；`let terms: [BTreeMap<String, u32>; 3] = Default::default();` —— 数组也实现了 `Default`。

## 3. `filter.rs` —— 过滤编译

**(a) 骨架**：把 `Filter` 针对**每个段**编译成 `Cond`（关键词值先换成该段的字典序号，段里没有这个值就直接「全不匹配」），`SegmentFilter::allows(doc)` = 未删除 && 所有条件为真。
**(b) 为什么**：惰性求值——只对候选文档判断，没过滤时零开销。另一种做法是先算出整段的位图（适合非常选择性的过滤，可以反过来驱动迭代），这里没做，BENCHMARK 里能看到高选择性过滤（1%）时剪枝效率下降的现象。
**(c) 语法**：`enum Cond<'a> { Ord(Option<Column<'a>>, Option<u32>), .., Not(Box<Cond<'a>>) }` —— 递归 enum 需要 `Box`（否则大小无限）。

## 4. `wand.rs` —— Block-Max WAND 和 MaxScore

**(a) 骨架**
- 公共部分：每个查询词一个 `Bm25TermScorer`（全局统计量），全局 top-k 堆跨段共享（段按顺序、文档号递增），每个词的**列表上界** `ub = max over blocks score(max_tf, min_doc_len)`。
- **BMW**：游标按当前文档排序 → 累加 ub 找 pivot（累计上界第一次超过门槛 θ 的位置）→ 用跳表算 pivot 所在块的上界之和（不解码）→ 如果块上界也超不过 θ，跳到最短块的末尾之后 → 否则所有游标都在 pivot 上就完整打分，不然把落后的游标推到 pivot。
- **MaxScore**：词按 ub 升序；θ 一旦确定，ub 前缀和 ≤ θ 的那些词是「非必要词」（non-essential）：只匹配它们的文档不可能进 top-k。候选只从必要词的 postings 里取；对每个候选，按 ub 从高到低去非必要词里 `advance_to` 查分，一旦「已得分 + 剩余上界」≤ θ 就放弃。

**(b) 为什么**
- **精确性（面试重点）**：三个条件保证剪枝版和穷举版**逐位相同**：① 分数按同样的词序求和（和 TAAT 累加器的 f32 加法顺序一致）；② 全局按文档号递增访问，分数等于 θ 的候选在平局规则下必输，所以「上界 ≤ θ 就剪」不丢东西；③ f32 舍入：剪枝条件是 `bound × (1 + 1e-5) ≤ θ`，留出远大于累计舍入误差的余量。用 proptest（多段、删除、过滤、各种 k）验证，并且做了**变异测试**：把上界故意缩小到 0.7 倍，测试立刻失败——证明测试真的能抓到不安全的剪枝。BEIR 5 个数据集上 run 文件和第一阶段逐字节相同。
- **性能故事（真实踩坑）**：
  1. 第一版 BMW 比穷举 TAAT 还慢（开发时在负载下的一次单次测量，scifact k=10：238µs vs 43µs；不是正式基准）。原因之一：每轮排序时移动的是游标结构体本身，每个游标带两个 128 元素的解码缓冲区（>1KB）。改成排序下标数组。
  2. `advance_to` 在跳表里线性扫描 → 改成二分；块上界加缓存。
  3. 还是不够：trec-covid 上 BMW 每个查询平均只完整打分 1,374 个文档（穷举要碰约 18 万个 posting），但主循环平均迭代 16,366 次，每次的排序/找 pivot/算块上界开销把收益吃掉了（`results/stage3/wand_debug_trec-covid_k10.txt`）。这些 BEIR 查询很长（6–11 个词），里面很多高频低 IDF 词（what/have/been）。
  4. 于是实现了 **MaxScore**：它把高频低权重词变成「非必要」，候选只来自少数稀有词。迭代数其实差不多（18,577），但每次迭代便宜得多（不排序、不算块上界），完整打分只有 338 个文档。正式基准：k=10 时 MaxScore 在 fiqa / scidocs / trec-covid 上比 TAAT 快 1.1–1.7 倍，在两个很小的数据集上反而更慢；BMW 在所有 k=10 配置下都比 TAAT 慢（0.47–0.71 倍）。所以默认用 MaxScore。Lucene 9.x 对析取查询用的也是（块级）MaxScore。
  5. 诚实结论：在这些小语料、长查询上，剪枝的收益有限；k=1000 时门槛太低，经常不如穷举。
- **为什么剪枝在小语料上收益有限**：TAAT 用稠密累加器、顺序解码，每个 posting 只要几纳秒；剪枝算法每一步有分支和随机跳转。只有当 posting 列表很长、k 很小时剪枝才明显划算。
**(c) 语法**：`impl Ord for Entry`（score 用 `total_cmp`，平局时反向比较文档号）；`BinaryHeap<Reverse<Entry>>` 小顶堆；`peek_mut` 原地替换堆顶；`cursors.sort_by(|a, b| a.ub.total_cmp(&b.ub))`；`Option<f32>` 数组 `contrib` 按词序暂存贡献再求和；`for s in contrib.iter_mut() { if let Some(v) = s.take() { .. } }` —— `Option::take` 取出值并留下 `None`，顺便清空。

## 5. `exec.rs` —— 布尔 / 短语 / 前缀执行器

**(a) 骨架**：`trait Matcher { advance_to(target) -> doc; score(doc_len) }`，三种实现：`TermMatcher`、`PhraseMatcher`（各词游标跳到同一文档后检查位置是否连续 p, p+1, ...，短语频率当 tf，IDF 取各词之和——Lucene PhraseQuery 的做法）、`PrefixMatcher`（FST 范围遍历展开成最多 1024 个词，并集，常数分 1.0——Lucene 默认的 constant-score rewrite）。`run`：有 must 时用 leapfrog 求交集，否则 should 求并集；must_not 命中则排除；分数 = must 分数 + 命中的 should 分数（按子句顺序）。
**(b) 为什么**：DAAT 每次只看一个文档，天然支持交集、排除和位置检查；穷举但不慢（交集会让候选少很多）。前缀展开超过上限就报错而不是悄悄截断（截断会让结果「看起来对」但其实不完整）。正确性：proptest 和一个直接在原始 token 上算的朴素实现对比（含短语、前缀、过滤、删除）。
**(c) 语法**：`Box<dyn Matcher + 'a>` —— trait 对象（动态分发），因为一个查询里混着不同类型的子句；`+ 'a` 说明它借用了段的数据。`let parts: Option<Vec<_>> = terms.iter().map(|t| seg.cursor(t)).collect();` —— `Option` 也能 `collect`：只要有一个 `None` 结果就是 `None`（短语里有词不存在 → 这个段不可能匹配）。

## 6. `bin/strata-qbench.rs`

从 BEIR 查询文本生成短语（前两个非停用词）、前缀（最长词前 4 个字母）、布尔（`+t1 t2 -t3`）、过滤（`bucket:[0 TO 9]`，合成字段 = FNV-1a(id) % 100）查询，测 p50/p99。`bucket` 是合成的（BEIR 没有元数据），文档里写明。

## Stage 3 面试问答

**Q1. WAND 的核心思想？pivot 是什么？**
每个词有一个分数上界。游标按当前文档号排序，从前往后累加上界，第一次超过当前第 k 名分数 θ 的位置就是 pivot。pivot 之前的文档最多只能匹配 pivot 之前的那些词，它们的上界加起来都不到 θ，所以可以直接跳过，把落后的游标推到 pivot 文档。

**Q2. Block-Max WAND 比 WAND 多了什么？**
上界细化到每个 128 文档的块：我们在跳表里存了每块的 `max_tf` 和 `min_doc_len`，所以 `score(max_tf, min_doc_len)` 是块内上界。pivot 通过了列表级检查之后，再用块级上界检查一次，不过就跳到块末尾，连块都不用解码。

**Q3. MaxScore 和 WAND 有什么区别？什么时候谁更好？**
WAND 每一步动态找 pivot；MaxScore 按上界把词静态分成必要/非必要两组（θ 变大时边界右移），候选只从必要词出。长查询、包含很多高频低权重词时 MaxScore 更好（我们的 BEIR 结果就是这样：trec-covid k=10 p50 809µs vs BMW 1,983µs vs TAAT 1,342µs，单段）；短查询、词权重接近时 BMW 的块级跳跃更有优势。

**Q4. 你怎么保证剪枝不改变结果？浮点数怎么办？**
见上文三个条件：同样的求和顺序、按文档号递增访问使平局必输、上界留 1e-5 的相对余量。测试：proptest 逐位比较 + 变异测试 + 5 个 BEIR 数据集 run 文件逐字节一致。

**Q5. 为什么一开始你的 WAND 比穷举还慢？怎么定位的？**
加了计数器（打分文档数、跳块次数、主循环迭代数）写了个诊断程序逐查询打印：打分文档只有穷举的 1%，但迭代数是打分数的 10–20 倍，说明开销在循环本身。修了「排序时移动大结构体」「跳表线性扫描」，然后换成更适合这些长查询的 MaxScore。教训：先测再猜；算法复杂度好不等于常数小。

**Q6. 短语查询怎么实现？为什么需要位置信息？**
先让各词游标对齐到同一文档（交集），再检查位置：对第一个词的每个位置 p，其它第 j 个词必须有位置 p+j。位置在第二阶段就写进了 `positions.bin`（varint 差值），按块记录偏移，顺序读取时是 O(总位置数)。

**Q7. 过滤为什么会让剪枝变差？**
过滤掉的文档不能进堆，θ 涨得慢，剪枝就弱；过滤越严格（1%），越接近穷举。更好的做法是选择性很高时反过来，用过滤结果（位图）驱动迭代。BENCHMARK 里有 1%/10%/50% 三档的数据：1% 时 MaxScore 比 TAAT 慢（trec-covid 1,964 vs 1,507µs），50% 时快 2.3 倍。

**Q8. 前缀查询为什么是常数分？为什么要限制展开数？**
一个前缀可能展开成上千个词，按 BM25 逐个打分既慢、语义也不明确（稀有词会主导分数），所以 Lucene 默认常数分。展开数上限防止 `a*` 这种查询拖垮服务；超过就报错，不静默截断。

---

# Stage 4：向量索引（Vector index）

> 新 crate：`crates/strata-vector`；脚本 `scripts/embed_beir.py`（本地算一次嵌入）、`scripts/hnswlib_baseline.py`（对照组）。
> 教学顺序：`distance.rs → vectors.rs → quant.rs → flat.rs → hnsw.rs → embeddings.rs → bin/strata-vbench.rs`

## 全局图

```
BEIR 文本 ──(Python, 一次性, MPS)──► bge-small-en-v1.5 ──► data/embeddings/<ds>/corpus.f32 (n × 384, 已归一化)
                                                                  │
          strata-vbench：精确暴力搜索（ground truth）─────────────┤
                         HNSW 建图（M=16, efC=200）──► 按 ef 扫描：recall@10 vs 延迟（f32 / int8+重打分）
                         过滤 ANN（1% / 10% / 50%）：图内过滤 vs 暴力扫描允许集合
                         稠密检索的 nDCG@10（给第五阶段混合检索用的 run 文件）
          hnswlib（同参数、单线程）对照
```

## 1. `distance.rs` —— 相似度内核

**(a) 骨架**：`dot`（f32）、`dot_i8`（i8→i32）、标量版本、aarch64 上的 NEON 版本（`std::arch::aarch64` 内联函数）。
**(b) 为什么**
- 向量都 L2 归一化了，所以**余弦相似度 = 点积**，省掉开方。
- **f32**：标量版本编译器不能自动向量化——浮点加法不满足结合律，重排顺序会改变结果，LLVM 必须保持顺序；手写 NEON（4 组 `vfmaq_f32` 累加器）实测快约 3 倍（BENCHMARK）。
- **int8 的意外发现（面试好故事）**：我手写了 NEON 的 int8 内核（`vmull_s8` 扩宽乘 + `vpadalq_s16` 成对累加），结果**比朴素标量循环慢一倍**。查汇编（`results/stage4/asm_dot_i8_scalar.s`）发现：整数加法满足结合律，LLVM 把标量循环自动向量化成了 `SDOT` 指令（M 系列芯片的 dotprod 扩展，一条指令做 16 个乘加），而 Rust 里 `vdotq_s32` 内联函数还是 unstable。于是 `dot_i8` 改用标量循环，手写版本保留做对照。教训：先测量，再相信「手写 SIMD 一定更快」。
- **`unsafe` 的隔离**：只在 `neon` 模块里，用 `// SAFETY:` 写清楚为什么不越界（循环边界保证 `p + 16 <= n`），proptest 和标量结果比对（f32 误差 < 1e-4，int8 完全相等）。

**(c) 语法**：`#[cfg(target_arch = "aarch64")]` 条件编译；`unsafe { vld1q_f32(pa.add(p)) }` —— 裸指针 `add` 偏移；`std::hint::black_box` 防止编译器把基准测试优化掉。

## 2. `vectors.rs` / `quant.rs` / `flat.rs`

**(a) 骨架**：`Vectors`：n×dim 连续 `Vec<f32>`；`ScalarQuantizer`：全局 scale = |x| 的 99.9% 分位数，`q = round(x/scale·127)`，近似点积 = scale²/127² × `dot_i8`；`exact_top_k`：暴力扫描 + 小顶堆。
**(b) 为什么**
- 连续存储：一次分配，扫描对缓存友好。
- **int8 标量量化（SQ8）**：内存 4 倍压缩、点积用整数指令；用分位数而不是最大值做 scale，防止少数离群坐标浪费 255 个量化级；因为是近似值，所以图搜索用量化相似度走，最后 ef 个候选**用 f32 原始向量重打分**（oversampling + rescoring，Qdrant 也是这么做）。
- 没做：按维度的 scale、乘积量化（PQ）——写明是限制。
- 暴力搜索是 recall 的**真值**；用 numpy 在 hnswlib 脚本里独立再算一遍真值，作交叉验证。
**(c) 语法**：`select_nth_unstable_by(idx, f32::total_cmp)` —— O(n) 选第 k 小（不用全排序）；`impl Ord for Neighbor`（相似度相同时 id 小的优先，结果确定）。

## 3. `hnsw.rs` —— HNSW 图

**(a) 骨架**
- 每个节点随机一个最高层 `level = floor(-ln(U) · mL)`，`mL = 1/ln M`（几何分布：越高层节点越少，形成「快速通道」）。
- 插入：从入口点在高层贪心下降 → 在每层用 `ef_construction` 做束搜索（beam search）→ 用**启发式（论文 Algorithm 4）**选邻居：候选按相似度从高到低，只有当它「离新节点比离所有已选邻居都近」才保留——让边分散到不同方向，图更可导航 → 反向加边，超过 M（第 0 层 2M）就对那个邻居重新做启发式裁剪。
- 搜索：高层贪心 → 第 0 层用 `ef` 宽度的最优优先搜索：候选堆（大顶）+ 结果堆（小顶，大小 ef），最好的候选都比结果里最差的还差就停。
- 过滤搜索：过滤掉的节点**照样遍历**（保持图连通），只是不进结果；允许集合很小时直接暴力扫描允许的文档。
- `SearchContext`：visited 标记用「纪元号」(epoch)，每次搜索只把 epoch+1，不用清空 n 大小的数组。
**(b) 为什么**
- 为什么 HNSW：图索引在高召回区间延迟最好；代价是内存（每节点 ~2M 个 u32 边）和不能真正删除。
- 参数：M=16、efC=200 是 hnswlib 常用默认；`ef` 是查询时的召回/延迟旋钮（BENCHMARK 有曲线）。
- 确定性：固定随机种子，同样输入建出同样的图（有测试）。
- 单线程建图：简单、确定；并行建图需要节点级锁（hnswlib 那样），没做。
**(c) 语法**：`BinaryHeap<Neighbor>` 和 `BinaryHeap<Reverse<Neighbor>>` 一个大顶一个小顶；泛型闭包参数 `F: Fn(u32) -> f32`（f32 版和 SQ 版共用同一套搜索代码，静态分发）；`allow: Option<&dyn Fn(u32) -> bool>`；`is_none_or`。

## 4. `bin/strata-vbench.rs` + Python 脚本

**(a) 骨架**：加载嵌入和 qrels → 精确搜索（真值 + 稠密 run 文件 + nDCG）→ 建图 3 次计时 → ef 扫描（f32 和 SQ8，各 3 遍计时）→ 过滤（1/10/50%）→ HNSW run 的 nDCG → 内核微基准。`embed_beir.py`：文档按长度排序后分批（快很多）、查询加 BGE 推荐的检索指令前缀、记录模型 revision 和版本；`HF_HOME` 指向 `data/hf`，不污染全局缓存。
**(b) 为什么**：召回必须对**同一个相似度、同一个归一化**下的精确结果算；hnswlib 对照用同样 M/efC、单线程、逐条查询（它的延迟包含一次 Python→C++ 调用，几微秒，原样报告）。

## Stage 4 面试问答

**Q1. HNSW 为什么快？复杂度大概多少？**
分层图：高层节点稀疏、边长，像跳表的快速通道，贪心几步就到目标附近；底层在局部用束搜索精修。经验上查询代价约 O(log n) 次「访问节点 × 每节点 M 个距离计算」，远小于暴力 O(n)。BENCHMARK 里有每次查询的距离计算次数。

**Q2. M、ef_construction、ef 分别影响什么？**
M：每个节点的边数 → 内存和图质量；efC：建图时的束宽 → 建图时间和图质量；ef：查询时束宽 → 召回和延迟的权衡旋钮（只影响查询，不用重建）。

**Q3. 邻居选择启发式是什么？为什么不直接选最近的 M 个？**
最近的 M 个常常挤在同一个方向（同一个簇），图在簇之间就断了。启发式只保留「离新节点比离已选邻居都近」的候选，等于每个方向留一条边，簇和簇之间有桥，贪心搜索不容易卡在局部。

**Q4. 怎么评估 ANN？recall 怎么定义？**
recall@10 = HNSW 返回的前 10 个里有多少在精确前 10 里。真值用暴力搜索，并且用 numpy 独立算一遍交叉验证。还要看下游指标：HNSW 的 nDCG@10 和精确搜索的差多少（BENCHMARK 有）。

**Q5. 标量量化的原理和代价？**
每个 float 映射到 int8（全局 scale），内存 4 倍压缩、整数点积快；误差会让图搜索的排序略有偏差，所以最后用原始 f32 重打分前 ef 个候选，召回几乎不掉（见曲线）。代价：还要存 f32 用于重打分（磁盘/内存层次分开时才真正省内存）。

**Q6. 过滤向量搜索的难点？你怎么做的？**
过滤很严格时，图里大部分节点不合格，HNSW 要么找不到足够结果，要么遍历大半个图。两种策略：图内过滤（不合格节点照样走但不进结果）和预过滤后暴力扫描允许集合。BENCHMARK 显示 1% 选择性时暴力扫描快得多，50% 时图搜索好，所以按允许集合大小切换（`search_filtered`）。

**Q7. 手写 SIMD 一定更快吗？**
不一定。f32 手写 NEON 快约 3 倍（编译器受浮点结合律限制不能向量化）；int8 手写 NEON 反而比标量慢一倍，因为编译器把整数标量循环自动向量化成了 SDOT。看汇编确认。

**Q8. HNSW 怎么删除？**
不能真正删除（删边会破坏连通性）。做法是打标记：节点留在图里继续当「路标」，但不返回；大量删除后重建。第六阶段的服务就是这么处理更新的。

---

# Stage 5：混合检索（Hybrid retrieval）

> 新文件：`crates/strata-eval/src/fusion.rs`、`stats.rs`；`strata-eval hybrid / compare` 子命令；`scripts/rerank_cross_encoder.py`、`crosscheck_stats.py`。
> 教学顺序：`fusion.rs → stats.rs → main.rs（hybrid/compare）→ rerank_cross_encoder.py`

## 全局图

```
BM25 run (top 1000, stage 1) ─┐
                              ├─► RRF(k=60) / min-max 加权(α=0.5) / z-score 加权(α=0.5) ─► 融合 run
dense run (top 1000, stage 4) ┘                                         │
                                                                         └─► cross-encoder 重排前 100（本地 MiniLM）
每个 run 的逐查询 nDCG@10 ─► 配对随机化检验 + 配对 t 检验（t 检验用 SciPy 交叉验证）
```

## 1. `fusion.rs`

**(a) 骨架**：`rrf(runs, k, depth)`：`Σ 1/(k + rank)`；`weighted(lex, dense, α, norm, depth)`：每个查询内对两个 run 的分数分别归一化（min-max 或 z-score），缺失文档取该 run 的最小归一化分（「这个检索器没给证据」），再 `α·lex + (1−α)·dense`。
**(b) 为什么**
- BM25 分数无上界，余弦相似度在 [−1, 1]，**不能直接相加**：要么只用排名（RRF），要么先归一化。
- RRF 不需要任何校准，k=60 来自原论文（Cormack 2009），对 k 不敏感；加权融合能利用分数差距，但多一个超参数 α。
- **防止「在测试集上调参」**：我们预先固定 k=60、α=0.5，α 的 0.0–1.0 扫描单独报告并明确标为「oracle 分析」，只说明敏感性，不当作结果。
**(c) 语法**：`HashMap::entry(doc).or_insert(0.0)` 返回可变引用，`*... += x` 累加；`fn finish<K: Into<String>>(acc: HashMap<K, f64>, ..)` —— 泛型参数让 `&str` 和 `String` 两种 key 共用一个函数。

## 2. `stats.rs` —— 显著性检验

**(a) 骨架**：`paired(a, b, samples, seed)` 同时做：
- **配对随机化检验**：零假设下每个查询的差值正负号等可能，随机翻转符号很多次，看平均差的绝对值有多少次 ≥ 观测值，p = (次数+1)/(样本数+1)。不需要正态假设，IR 领域推荐（Smucker 等 2007）。
- **配对 t 检验**：t = mean(d)/(sd(d)/√n)，用 Student t 分布算双侧 p；t 分布 CDF 用正则化不完全 Beta 函数（连分式，Lentz 方法）+ Lanczos 近似的 lnΓ 自己实现，用已知临界值做单元测试，并在脚本里和 SciPy 的 `ttest_rel` 对比（一致到 1e-6）。
**(b) 为什么**：BEIR 某些数据集只有 50 个查询（trec-covid），0.01 的 nDCG 差别很可能是噪声；做 25 个检验还要考虑多重比较（我们注明了 Bonferroni 阈值）。
**(c) 语法**：手写 xorshift 随机数（不引入 rand 依赖、结果可复现）；`const C: [f64; 9]` 常量数组。

## 3. `rerank_cross_encoder.py`

**(a) 骨架**：读 RRF 的 run，取每个查询前 100，用 `cross-encoder/ms-marco-MiniLM-L-6-v2` 给 (查询, 文档) 对打分，重排前 100，100 名以后保持原顺序。
**(b) 为什么**：双塔（bi-encoder）模型把查询和文档分别编码，无法做 token 级交互；cross-encoder 把两者拼在一起过 Transformer，准确得多但每对都要一次前向，只能用在少量候选上——典型的「召回（便宜、广）+ 重排（贵、准）」两阶段架构。
- 在 Rust 里跑神经网络需要 ONNX Runtime/candle，这一阶段选择 Python 离线做，结果写成 run 文件，Rust 负责评测（写明了）。

## Stage 5 面试问答

**Q1. RRF 的公式？为什么 k=60？**
score(d) = Σ_r 1/(k + rank_r(d))。k 越大，高排名和低排名的差距越小；k=60 是原论文在多个 TREC 数据上的经验值，结果对 k 不太敏感。它只看名次，所以天然适合融合分数尺度完全不同的检索器。

**Q2. 为什么不能直接把 BM25 分数和余弦相似度加起来？**
尺度不同：BM25 随查询词数、IDF 变化，可以是 5 也可以是 50；余弦在 −1 到 1。直接相加时 BM25 会主导。要么归一化（min-max 对极端值敏感，z-score 假设分布形状），要么只用排名（RRF）。

**Q3. 缺失文档怎么处理？**
一个文档只出现在一个 run 里时，另一个 run 给它「最低分」（min-max 下就是 0），意思是「这个检索器认为它不相关」。这是一个设计选择，会影响结果；我们写在代码注释里。

**Q4. 你怎么判断混合检索真的更好，而不是噪声？**
逐查询配对检验：同一组查询上两个系统的 nDCG@10 做配对随机化检验和 t 检验；t 检验和 SciPy 交叉验证。25 个检验要考虑多重比较：Bonferroni 后阈值是 0.002。小数据集（trec-covid 50 个查询）上差别要大很多才显著。

**Q5. 为什么 α 不在测试集上调？**
在测试集上选最好的 α 是「用答案调参」，报告出来的数字会偏高（乐观偏差）。正确做法是在验证集（dev split）上调或预先固定。我们预先固定 α=0.5，扫描只作为敏感性分析。

**Q6. cross-encoder 和 bi-encoder 的区别？为什么只重排前 100？**
bi-encoder：查询和文档分别编码成向量，文档向量可以离线算好、建 ANN 索引，查询时只算一次向量——快，可用于召回。cross-encoder：每个 (查询, 文档) 对一起编码，能看到词与词的交互，准但每对一次前向推理，成本 O(候选数)。所以先用便宜的召回出 100 个，再用贵的重排。

**Q7. 混合检索在什么情况下帮助最大？**
当两个检索器犯的错不一样、且水平接近时：BM25 擅长精确词匹配（专有名词、编号），稠密检索擅长同义改写。我们的数据：trec-covid 上 RRF 比稠密高 0.046（p=0.027），scifact/nfcorpus 也是融合最好；但 fiqa 上 BM25（0.235）远差于稠密（0.404），RRF 反而比稠密低 0.041（显著）——弱的检索器把强的拖下来了。

**Q7b. 为什么 cross-encoder 重排反而变差？**
实测 MiniLM-L-6（只在 MS MARCO 网页段落上训练）在 5 个数据集里有 4 个让 nDCG@10 下降（scifact −0.045）。推测（没验证）：小模型 + 领域迁移（科学/医学文本）不如训练数据更广的 bge-small。教训：重排不是免费的提升，要在自己的数据上测。

**Q7c. 你在实现融合时踩过什么坑？**
扫 α 时发现 α=1 本应等于纯 BM25，nfcorpus 上却是 0.2985 而不是 0.3202。原因：只有一个命中的查询，min-max 归一化分母为 0，那个命中被归一成 0，和所有「没检索到」的文档并列。修复：退化列表归一成 1，缺失文档的分数严格低于最低分；加了「α=1/α=0 精确复现输入排序」的单元测试。

**Q8. 这个阶段有哪些局限？**
融合在 run 文件上离线做（服务里的在线 RRF 在第六阶段）；重排在 Python 里跑；α、k 没有在 dev 集上调；只用了一个小的嵌入模型和一个小的重排模型。

---

# Stage 6：单机服务（Single-node service）

> 新 crate：`crates/strata-server`（`engine.rs`、`vector_store.rs`、`http.rs`、`grpc.rs`、`metrics.rs`、`telemetry.rs`、`bin/strata-server.rs`、`bin/strata-load.rs`、`proto/strata.proto`、`build.rs`）。
> 教学顺序：`engine.rs → vector_store.rs → http.rs → grpc.rs (+ proto/build.rs) → metrics.rs → telemetry.rs → bin/strata-server.rs → bin/strata-load.rs`

## 全局图

```
           HTTP/JSON (axum)              gRPC (tonic, 由 .proto 生成)
                 │                                  │
                 └──────────────► Engine ◄──────────┘
                                   │  ingest: 先 vectors.log fsync, 再词法 WAL fsync, 然后 ack
                                   │  search: lexical (查询语言 + MaxScore) / dense (HNSW) / hybrid (RRF)
                 ┌─────────────────┴──────────────────┐
          strata-storage Index                VectorStore (vectors.log + HNSW)
   /metrics (Prometheus) · tracing spans (OpenTelemetry: OTLP 或 stdout)
   strata-load：闭环 (N 个并发 worker) / 开环 (固定速率调度, 无协同遗漏)
```

## 1. `engine.rs`
**(a)** 把第二～五阶段拼起来：`ingest`、`delete`、`commit`、`search(mode)`；hybrid 模式各取前 100 做 RRF。
**(b)** 前端（HTTP、gRPC）只是翻译层，业务逻辑只写一份；`#[tracing::instrument]` 给 ingest/search 自动生成 span。
**(c)** `#[serde(default)]`、`#[serde(rename_all = "lowercase")]` 让 JSON 字段可选、枚举用小写字符串。

## 2. `vector_store.rs`
**(a)** 追加写的 `vectors.log`（CRC 记录，批量 fsync）+ 内存 HNSW；启动时重放日志重建图；upsert 追加新节点、旧节点标记删除；`SearchContext` 池复用 visited 数组。
**(b)** HNSW 不能真删除；图重建是 O(n log n) 的启动成本（BENCHMARK 有建图时间）。向量和词法 WAL 是两个文件，不是一个原子提交——写明了。
**(c)** `RwLock<State>`：搜索拿读锁并发，插入拿写锁；`Mutex<Vec<SearchContext>>` 简单对象池。

## 3. `http.rs` / `grpc.rs`
**(a)** axum 路由 + `State<AppState>` 依赖注入；所有可能阻塞（fsync）或 CPU 密集的引擎调用放到 `spawn_blocking`，不卡住异步 worker 线程。gRPC 用 `.proto` 定义，`build.rs` 里用 protox（纯 Rust 的 protobuf 编译器，不需要安装 protoc）+ tonic-prost-build 生成代码。
**(b)** 为什么两种 API：HTTP/JSON 方便调试和脚本；gRPC 有强类型契约、HTTP/2 多路复用，适合服务间调用（第七阶段协调器）。错误统一转成 400 + JSON 消息。故障注入（按概率延迟搜索）给第七阶段测尾延迟用。
**(c)** `impl<E: Into<anyhow::Error>> From<E> for ApiError` —— 一个泛型 From 实现，让 handler 里可以直接用 `?`；`#[tonic::async_trait]`；`tonic::include_proto!`。

## 4. `metrics.rs` / `telemetry.rs`
**(a)** Prometheus：请求计数（按路由、状态）、延迟直方图（50µs 起翻倍 17 档）、已摄入文档数、存活文档数、段数、向量数；`/metrics` 文本格式。Tracing：`tracing-subscriber` 日志 + 可选 OpenTelemetry layer（OTLP/gRPC 批量导出，或 stdout 导出用于验证）。
**(b)** 直方图而不是平均值：p99 才是用户感受到的；分桶要覆盖从微秒级搜索到秒级批量写入。OTLP 导出需要一个 collector，本机没有跑，所以用 stdout 导出器验证 span 确实产生（写明 OTLP 路径未在本机端到端验证）。

## 5. `bin/strata-load.rs`
**(a)** 闭环：N 个 worker 收到响应才发下一个——测容量；开环：按固定速率调度请求，延迟从「计划发送时间」算起——避免**协同遗漏（coordinated omission）**：服务卡住时闭环压测器也跟着停发，卡顿就从统计里消失了。
**(b)** 预热期（warmup）不计入；3 次重复报均值和标准差；压测器和服务在同一台机器上，会互相抢 CPU（写明）。
**压测器本身的坑（真实经历，面试好故事）**：
1. 第一版开环用 `tokio::time::sleep_until` 调度，tokio 定时器精度约 1ms，结果开环 p50 是 1.5–1.8ms，而闭环单并发只有 0.48ms——多出来的是**压测器自己迟到**，不是服务慢。
2. 改成「睡到前 1.5ms 再自旋」后 p50 回到 ~0.5ms，但 ≥2000 req/s 时尾延迟暴涨、还出现错误：自旋发生在 tokio 的 worker 线程上，**饿死了处理响应的任务**。加了「发送延迟」（实际发出时间 − 计划时间）统计，p99 达到 31ms，证实了是压测器的问题。
3. 最终把调度器放到独立 OS 线程上自旋，用 `Handle::spawn` 把请求投递给运行时。
三版数据都保留在 `results/stage6/superseded_openloop/`，BENCHMARK 只用最后一版，并报告发送延迟。教训：先验证测量工具本身。

## Stage 6 面试问答
**Q1. 为什么要 spawn_blocking？** tokio 的 worker 线程数≈核数，在上面做 fsync 或几毫秒的 CPU 计算会让同一线程上的其他请求排队；blocking 池专门跑这种任务。
**Q2. 闭环和开环压测的区别？什么是 coordinated omission？** 见上。开环的 p99 才反映「用户按自己的节奏来请求」时的真实尾延迟。还要验证压测器自己准不准：我们记录了「发送延迟」。
**Q3. 怎么选 Prometheus 直方图的桶？** 按预期延迟范围取指数桶；桶太粗 p99 估不准，太细时间序列太多。我们 50µs×2^k，到约 3.3s。
**Q4. gRPC 和 REST 怎么选？** 内部服务间用 gRPC（契约、二进制、HTTP/2、流式）；对外/调试用 HTTP/JSON。
**Q5. 为什么写入要两个 fsync？能合并吗？** 向量日志和词法 WAL 分开；可以把向量放进同一个 WAL 记录里实现原子性和一次 fsync，是已知的改进点。
**Q6. 吞吐量的瓶颈在哪？** 闭环 64 并发时词法搜索约 11,800 req/s、p50 5.1ms：8 核被服务和压测器一起打满；64 / 11,840 ≈ 5.4ms，正好符合利特尔定律（Little's law：并发 = 吞吐 × 延迟）。写入的瓶颈是 HNSW 单线程插入（向量库写锁），带向量摄入约 2,200 docs/s。
**Q7. 你的 tracing 是怎么验证的？** 用 stdout 导出器跑一次真实请求，检查输出里有 HTTP→engine.search 的 span 和属性。
**Q8. 服务的局限？** 单进程单机；没有认证、限流、背压；向量和词法写入不是一个原子提交；HNSW 启动时重建。


---

# Stage 7：分布式（Distributed）

> 新文件：`crates/strata-query/src/stats.rs`（`ScoringStats`）、`Snapshot::search_scored`、服务端 `/v1/stats` 和 `global_stats`、`crates/strata-server/src/bin/strata-coord.rs`（协调器）、`strata-load run-queries`、`scripts/run_stage7.sh`、`scripts/tie_analysis.py`、`scripts/make_tables_stage7.py`。
> 教学顺序：`stats.rs → snapshot.rs::search_scored → engine.rs (plain / global_stats / scoring_stats) → strata-coord.rs`

## 全局图

```
client ──► strata-coord
            │ 写：按 FNV-1a(id) % 分片数 路由，写到该分片的所有副本，全部 ack 才 ack
            │ 读：第 1 阶段 /v1/stats 并行发给每个分片 → 求和得到全局 N、总词数、df
            │     第 2 阶段 /v1/search 带全局统计量并行发给每个分片 → 各分片 top-k
            │     合并：按分数降序（分片文档互不相交）取前 k
            │     每个分片调用有截止时间；超时/失败 → partial=true + failed_shards
            │     对冲请求（hedging）：主副本 hedge_ms 内没回 → 再发给另一个副本，谁先回用谁
   shard0 [r00, r01]   shard1 [r10, r11]  ...（每个都是第六阶段的 strata-server）
```

## 1. `stats.rs` + `search_scored`
**(a)** `ScoringStats { num_docs, total_tokens, df }`，`local(snapshot, terms)`、`merge(parts)`；所有打分路径（MaxScore、BMW、TAAT、布尔执行器）都有 `*_with_stats` 版本。
**(b)** **全局 IDF**：BM25 的 IDF 和 avgdl 依赖整个集合。每个分片只用本地统计量，同一篇文档在不同分片的得分就不同，合并 top-k 会排错。两阶段（先收集统计量再打分）是 Elasticsearch `dfs_query_then_fetch` 的思路；代价是多一轮网络往返。我们在第一阶段就把 `CollectionStats` 和段分开、第二阶段做了多段全局统计，这里只是把「段」换成「机器」。
**(c)** `impl<'a> IntoIterator<Item = &'a str>` 参数；`HashMap::entry().or_default()`。

## 2. `strata-coord.rs`
**(a)** axum 服务；`call_shard`：`tokio::select!` 实现对冲——主请求和一个 `sleep(hedge)` 赛跑，睡眠先结束就再发备份请求，然后两个请求赛跑；外面再套 `tokio::time::timeout` 做截止时间；`futures::future::join_all` 并发调用所有分片。
**(b)**
- 为什么哈希分片：均匀、无需目录服务；代价是范围查询/扩容要重分布（一致性哈希能减少迁移，未做）。
- 为什么对冲：尾延迟放大——一次查询要等**所有**分片，任何一个副本偶尔慢（GC、磁盘、邻居干扰）都会拖慢整次查询；分片越多越明显（Dean & Barroso《The Tail at Scale》）。对冲用少量额外请求换短得多的尾巴；实测（BENCHMARK 第七阶段）：对冲阈值 5 ms 时额外分片请求 1.67%，15 ms 时 1.26%；p99 从 56.3 ms 降回 9.3 ms（健康基线 9.0 ms）。
- 部分结果：可用性优先——宁可少一个分片的结果，也不要整个请求超时；但要明确标出来（`partial`）。统计量缺一个分片时分数也只是近似。
**(c)** `tokio::pin!` 把 future 固定在栈上以便多次 `select!` 引用；`AtomicU64` 计数器统计对冲次数。

## Stage 7 面试问答
**Q1. 为什么分布式搜索要两阶段？** 见上：全局 IDF。单阶段（各分片用本地 IDF）在数据分布均匀时误差小但不为零；我们要求和单机**逐位一致**，所以必须两阶段。BENCHMARK 有 5 个数据集的验证。
**Q2. 分片 top-k 怎么合并？要每个分片返回多少？** 每个分片返回自己的 top-k，全局 top-k 一定在这些候选的并集里（任何全局前 k 的文档在它所在分片里也是前 k），所以每片取 k 就够，合并是 k·分片数 的排序。注意平局：截断处多个同分文档，每个分片自己挑，所以合并后的集合可能和单机不同（分数序列完全相同）。要逐字节一致就得全局统一 tie-break（比如都按外部 id）。
**Q3. 什么是 tail at scale？对冲请求的代价？** 扇出越大，「至少一个慢」的概率越高：单副本 p99 慢的概率 1%，100 个分片时一次查询碰到慢副本的概率约 63%。对冲：额外请求数 ≈ 超过对冲阈值的请求比例。我们实测 5 ms 阈值多了 1.67% 请求，备份请求 85% 先回；15 ms 阈值多 1.26%，但 p99 只能降到 18.2 ms（≈ 15 ms 等待 + 一次正常响应）——阈值越小尾巴越短、成本越高。风险：系统整体过载时对冲会放大负载，所以要限流（未做）。
**Q4. 写入怎么保证副本一致？** 这一阶段协调器同步写所有副本、全部成功才 ack——没有共识，协调器崩溃或副本写失败会让副本分叉。第八阶段用 Raft 解决。
**Q5. 超时和部分结果如何选择？** 截止时间按 SLA 设定（例如 p99 的几倍）；超时的分片计入 failed_shards；客户端可以决定接受部分结果或重试。
**Q6. 为什么用 FNV 哈希？** 简单、快、确定（不能用 Rust 默认的随机化 SipHash，那样每次进程启动分片不同）。
**Q7. 你怎么证明分布式结果和单机一样？** proptest：随机语料哈希分片、合并全局统计量，每篇文档分数和单个大索引逐位相等；BEIR：4 分片集群跑全部测试查询（k=1000），5 个数据集的 nDCG@10/R@100/R@1000/MRR@10 和单机到小数点后 6 位相同，top-100 全部一致、分数差为 0。top-1000 有 133/2306 个查询不同——`tie_analysis.py` 证明全是第 1000 名处「同分文档选哪个」的差别（每个分片各自打破平局）。
**Q8. 本阶段局限？** 所有进程跑在一台笔记本上（没有真实网络延迟和故障）；副本靠协调器双写（无共识）；没有重平衡/扩容；没有对冲限流；dense/hybrid 的跨分片合并只是按相似度合并各分片 HNSW 结果。

## 3. `scripts/tie_analysis.py`
**(a)** 读两个 TREC run，对每个不同的查询检查：逐名次的分数序列是否相同；除截断处那一组同分外，每个分数组里的文档集合是否相同。有一个查询不满足就以非零退出。
**(b)** 「结果不一样」时不要只说「大概是平局」——写脚本证明。这是本阶段 parity 结论能站住的关键：差异 100% 可解释，且指标完全相同。
**(c)** `collections.defaultdict(set)` 按分数分组；分数用字符串比较（run 文件里已是最短往返格式，避免浮点误差）。

---

# Stage 8：复制（Replication，自己写的 Raft）

> 新文件：`crates/strata-raft/src/core.rs`（算法）、`storage.rs`（持久化）、`server.rs`（复制分片）、`bin/strata-raft-node.rs`、`bin/strata-raft-chaos.rs`、`tests/sim_tests.rs`、`scripts/raft_mutations.sh`、`scripts/run_stage8.sh`、`scripts/make_tables_stage8.py`。
> 教学顺序：`core.rs → tests/sim_tests.rs → storage.rs → server.rs → strata-raft-chaos.rs`

## 全局图

```
client ─POST /v1/documents─► leader: propose(cmd) → 日志追加
            Ready 顺序：① 持久化（先 term/vote，再日志，fsync） ② 发消息 ③ 交给 applier 线程
            followers: 收到 Append → 一致性检查 → 追加+fsync → 回 AppendReply
            leader: 多数派 matched ≥ i 且 term(i)==当前 term → commit=i
            applier 线程按顺序把已提交的条目应用到 strata Index → 唤醒等待的写请求 → 200
读：?consistent=true → ReadIndex（一轮多数派心跳确认我仍是 leader）→ 等 applied ≥ readIndex → 读
```

## 1. `core.rs`（纯状态机）
**(a)** `RaftNode` 没有线程、时钟、I/O：`tick()` 推进逻辑时间，`step(Envelope)` 处理消息，`propose()` 追加命令，`read_index()` 开始线性一致读；所有「要做的事」放进 `Ready{hard_state, truncate_from, append, messages, committed, reads}`，由调用者按顺序执行。
**(b)**
- 为什么纯状态机（etcd/raft 的设计）：可以在测试里完全控制消息顺序、丢包、崩溃，跑几百种随机调度并检查安全性；网络/磁盘代码和算法分开，各自好测。
- 选举：随机超时（防止一直平票）；投票要求候选人日志「至少一样新」（先比最后一条的 term，再比 index）——否则会选出缺少已提交条目的 leader（变异测试 `no-up-to-date-check` 证明测试能抓到）。
- 提交规则（论文 Figure 8）：leader 只能通过「数副本」提交**当前任期**的条目；旧任期条目随之间接提交。所以新 leader 一上任就追加一条空的 no-op。
- 冲突回退：follower 拒绝时给 hint（冲突任期的第一个 index），leader 一次跳过整个任期。
- 只有让 `matched` 前进的回复才继续发下一批——否则每次心跳都开一条新的追赶链，消息数爆炸（模拟里见过 >10,000 条在途消息）。
**(c)** `std::mem::take(&mut self.ready)` 取走并重置；`HashMap<NodeId, Progress>`；`matches!` 宏；xorshift 随机数（确定性、可复现）。

## 2. `tests/sim_tests.rs`（随机模拟 + Figure 8 回放）
**(a)** 模拟网络：随机顺序投递（乱序）、10% 丢包、5% 重复、分区；节点崩溃后**只用持久化的状态**重启。检查：每个任期最多一个 leader；所有节点第 k 个应用的条目相同；leader 报告提交的条目最终不变；ReadIndex 读的 index ≥ 读开始前所有已确认写；恢复后能继续提交。
**(b)**
- 为什么变异测试：测试通过不代表测试有用。故意注入 6 个经典 bug，6/6 都让测试失败。第一版只抓到 3 个——分区太短（选不出新 leader）、批量 64 掩盖了部分复制；改成按种子变化分区时长和批量 1/2/64 后抓到 5 个；Figure 8 那个随机调度很难碰到，所以写了确定性回放。
- 网络模型也要合理：一开始每步只投递 1 条消息，消息产生得比投递快，积压上万条，所有投票都超时——测不出任何东西。改为容量随积压增长（每 100 条积压多投递 1 条）。
**(c)** `assert!(..., "seed {}")` 把种子写进失败信息，`SIM_SEED=n` 可单独重放。

## 3. `storage.rs`
**(a)** `hard_state.json`（tmp + fsync + rename + 目录 fsync）；`raft.log` 每条 `len | crc32 | JSON`；打开时遇到坏的尾巴就截断。
**(b)** 先写 hard state 再写日志：保证磁盘上的 term 永远 ≥ 日志里任何条目的 term。截断尾巴是安全的：没 fsync 完成的条目从没被确认给任何人。
**(c)** `File::set_len` 截断；`OpenOptions::truncate(false)` 明确不清空。

## 4. `server.rs`
**(a)** axum：`/raft/msg`（节点间）、`/v1/documents`（写，非 leader 返回 421 + leader 地址）、`/v1/documents/{id}?consistent=true`、`/raft/status`。applier 线程通过 channel 接收已提交条目，连续 upsert 合并成一批写进 Index。
**(b)** 写请求要等「提交 + 本机应用」才返回 200。失败/超时的写是**不确定**的（可能之后仍会提交），所以混沌测试只检查已确认的写。第一版在 Raft 锁里应用条目：重启的节点重放几千条（每条一次 fsync）时无法投票，故障转移时间涨到 17 秒。
**(c)** `std::sync::mpsc::channel` + 专用线程；`tokio::sync::watch` 发布 applied index；`oneshot` 回复等待的写请求。

## 5. `strata-raft-chaos.rs`
**(a)** 3 个节点进程；写线程不停写；每轮杀 leader（SIGKILL）→ 计时到「另一个副本」确认写 → 在新 leader 上一致读被杀 leader 最后确认的 50 个 id → 重启 → 等全部追上 → 下一轮。最后逐个核对所有已确认 id。
**(b)** 测量本身也会错：第二版把「杀之后收到的第一个 200」当成故障转移结束，但那可能是旧 leader 死前发出的回复——得到 10-30 ms 这种不可能的数字（选举超时最少 150 ms）。修正：必须是别的副本确认。
**(c)** `std::process::Child::kill()`（SIGKILL）+ `wait()`。

## Stage 8 面试问答
**Q1. 为什么自己写 Raft 而不用 openraft？** 学习和可解释性：每一行都能讲；纯状态机可以做确定性模拟测试。代价：没有快照/日志压缩、成员变更、pre-vote——生产环境我会用成熟实现。
**Q2. 什么是 Figure 8 问题？** 旧任期的条目即使已经复制到多数派，也可能被后来的 leader 覆盖（因为那个 leader 的最后一条 term 更大）。所以只按当前任期条目计数提交。测试 `figure_8_old_term_entry_is_not_committed_by_counting` 一步步重放这个场景。
**Q3. 怎么保证已确认的写不丢？** 先 fsync 再发消息/回复；多数派持久化才算提交；选举要求日志最新，所以新 leader 一定有所有已提交条目。实测 75 次杀 leader、53,130 个已确认 id，丢失 0。
**Q4. leader 上的读为什么不一定线性一致？** 被网络隔离的旧 leader 不知道自己已经被替换，会返回旧数据。ReadIndex：记下 commit index，发一轮心跳得到多数派确认，再等本地应用到那个 index。还有一个坑：新 leader 在本任期第一次提交前不知道真正的 commit index——我们的模拟测试就是在这里抓到了一个真实的 bug。
**Q5. follower 读会怎样？** 可能读到旧数据：实测确认后立刻在另一个副本普通读，3.9% 读不到。想要 follower 线性一致读，需要 follower 向 leader 要 ReadIndex。
**Q6. 故障转移时间由什么决定？** 选举超时（150-300 ms）+ 一轮投票 + 一次提交。实测 p50 261 ms、p90 448 ms、最大 782 ms；尾巴可能是平票需要再等一轮超时（没有专门测量确认）。缩短办法：更短的超时（误判风险更大）、pre-vote、主动 leadership transfer。
**Q7. 节点重启后怎么恢复？** 从 Raft 日志重建索引：commit index 从 0 开始，leader 告诉它真正的 commit，applier 重放。没有快照所以时间随日志增长（这里 p50 257 ms）。生产需要定期快照 + 截断日志。
**Q8. 你怎么知道测试能抓到 bug？** 变异测试：注入 6 个已知 bug（Figure 8、投票检查、重复投票、过期 ReadIndex、不经多数派的读、follower 越界提交），6/6 被抓。还保留了两次被推翻的混沌测试结果和原因——证明数字是可信的过程比数字本身更重要。
