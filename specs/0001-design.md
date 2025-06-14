
# **基于 Google Zanzibar 论文的简化版 Rust 授权系统设计报告**

## **1. 简化版 Rust Zanzibar 介绍**

Google 的 Zanzibar 是一个全球一致、高可用性的授权系统，旨在确定在线用户是否有权访问数字对象。其核心功能是存储和评估访问控制列表（ACLs）1。Zanzibar 提供了一个统一的数据模型和强大的配置语言，使得 Google 数百个客户端服务（包括日历、云端硬盘、地图、照片和 YouTube）能够表达广泛的访问控制策略 1。原始的 Zanzibar 系统在设计时设定了严格的目标：正确性（确保决策的一致性）、灵活性（支持丰富的访问控制策略）、低延迟（快速响应，特别是长尾延迟）、高可用性（可靠响应）和大规模（为数十亿用户保护数十亿对象，并在全球部署）1。

Zanzibar 的核心是将权限表示为“关系元组”，格式为 (object#relation@user)。这个基本单元可以表示直接的用户-对象关系，也可以引用其他用户集（例如，组），从而实现嵌套权限 1。复杂的访问控制策略，例如继承或不同权限类型之间的关系（例如，编辑者也是查看者），是通过命名空间配置中的“用户集重写规则”定义的，而不是通过显式存储元组 1。

本简化版 Rust 实现的设计目标旨在忠实地捕捉 Zanzibar 的核心授权逻辑，同时规避其大规模分布式系统的复杂性。

本简化版 Rust 实现的主要目标包括：

* **核心模型忠实度：** 首要目标是精确地表示 Zanzibar 的基本数据模型，包括关系元组、命名空间配置和复杂的用户集重写规则。这确保了 Zanzibar 授权引擎的逻辑本质得以保留。
* **DSL 兼容性：** 一个关键要求是设计一个清晰的、基于文本的领域特定语言（DSL），用于定义访问控制策略。该 DSL 将直接受到 Zanzibar 配置语言的启发，并与之兼容，从而实现人类可读且易于管理的策略定义。
* **本地执行焦点：** 为了简化实现并使其适用于单个开发人员或小型团队，设计将优先考虑单节点、内存中的执行模型。这种方法非常适合本地开发、测试以及不需要全球分布式的小规模应用程序。
* **清晰的实现路径：** 本报告将提供一个分阶段、循序渐进的路线图，指导开发人员从基础数据结构到核心逻辑和 DSL 集成，从而在 Rust 中构建系统。

为了实现上述目标，本设计明确地在以下方面做出了妥协和范围限制：

* **分布式方面：** 原始的 Zanzibar 在全球范围内运行，将数据复制到数十个数据中心，并将负载分配到全球数千台服务器上 1。本 Rust 实现将
  **不**是分布式系统。它将作为一个单一进程运行，完全抽象掉与网络通信、数据复制、分布式共识和负载均衡相关的复杂性。
* **全局一致性机制：** Zanzibar 主要通过依赖 Google 的 Spanner 数据库及其 TrueTime 机制以及“zookies”来实现强大的一致性保证，例如外部一致性和有界陈旧度的快照读取 1。本简化版 Rust 将
  **省略** zookies 和任何复杂的并发控制协议。授权检查将基于内存中最新可用状态进行操作，这意味着更新将采用“最终一致性”模型，而不是原始系统严格的外部一致性。
* **高级性能优化：** Zanzibar 采用了复杂的优化技术来实现其严格的延迟和可用性目标，包括分布式缓存、热点缓解、请求对冲以及用于深度嵌套集合的专用 Leopard 索引系统 1。这些复杂的优化将
  **排除**在本初始简化设计之外。重点将放在授权评估的逻辑正确性上，而不是高吞吐量或超低长尾延迟。
* **持久性：** 尽管生产环境中的 Zanzibar 系统使用 Spanner 进行持久存储 1，但本初始设计将使用内存数据存储。然而，将定义一个
  TupleStore 特性（trait），以便将来可以与简单的本地持久化解决方案（例如 SQLite、平面文件）集成，而无需完全重写核心授权逻辑。
* **Watch API/变更日志：** Watch API 提供了元组修改事件流，其底层变更日志数据库 1 主要用于在分布式环境中维护辅助索引。这些组件将从简化范围中省略。

通过对 Zanzibar 论文的分析，可以明确区分系统的“核心逻辑”与“分布式基础设施”。论文的第 2 节“模型、语言和 API”描述了 Zanzibar 的抽象概念，即系统“做什么”（数据模型、策略语言、核心操作）。相比之下，第 3 节“架构和实现”详细说明了 Google 如何在全球范围内实现这些功能（Spanner、Leopard、缓存、一致性协议）1。这种原始设计中固有的模块化结构表明，逻辑模型可以独立于其分布式基础设施进行提取和实现。这种对核心逻辑的关注使得项目变得可行，允许构建一个功能正确的授权引擎，而无需处理分布式系统工程的巨大复杂性。

此外，对第 2.3.1 节“关系配置和用户集重写”1 的深入研究揭示了

Userset Rewrite Rules 对于实现“DSL 兼容性”和策略灵活性的关键作用。这些规则是定义“有效 ACLs”和“对象无关关系”的核心机制，它们详细说明了 this、computed_userset、tuple_to_userset 等原语以及集合代数运算符（union、intersection、exclusion）的使用方式。这意味着，实现“DSL 兼容性”需要设计一种文本语法，能够直接解析为精确表示这些特定重写规则及其树状组合的 Rust 数据结构。没有对 UsersetExpression 的健壮建模，DSL 将是肤浅的，无法捕捉 Zanzibar 的核心灵活性。

最后，为了在存储方面做出妥协，同时又不牺牲未来的可扩展性，定义一个 TupleStore 特性至关重要。原始的 Zanzibar 依赖 Spanner 进行存储 1，而本简化模型将首先使用内存中的

HashMap。然而，如果在整个 Check 和 Expand 函数中直接嵌入 HashMap 逻辑，将导致紧密耦合，使得将来引入持久性变得困难。Rust 的特性系统非常适合通过定义一个接口来解决这个问题，具体实现可以遵循该接口。通过定义 TupleStore 特性，核心授权逻辑与底层存储机制解耦，从而提供了显著的可扩展性和可维护性。这意味着可以从简单的内存解决方案开始，然后通过实现新的 TupleStore 具体类型来升级到持久的本地存储层，而无需更改基本的 Check 和 Expand 算法。

## **2. Zanzibar 核心数据模型在 Rust 中的实现**

在 Zanzibar 中，访问控制列表（ACLs）被表示为“关系元组”的集合。这些元组表达了对象与用户之间，或对象与对象-关系对（用户集）之间的关系 1。

(object#relation@user) 格式是系统中权限的原子单元。

### **关系元组：基本构建块**

一个关系元组包含三个主要组成部分：一个 object、一个 relation 和一个 user。user 组件特别灵活，它可以是一个直接的 user_id（例如，一个独立用户）或一个 userset，而 userset 本身是另一个 object#relation 对（例如，group:eng#member）。这种 userset 能力对于表示嵌套组员关系和其他间接权限至关重要 1。论文指出，识别唯一关系元组所需的主键是

(namespace, object_id, relation, user) 1。

在 Rust 中，这种灵活的用户表示可以通过枚举（enum）类型优雅地实现。通过定义一个 User 枚举，它包含 UserId(String) 和 Userset(Object, Relation) 两种变体，Rust 的类型系统本身就强制了正确的结构，并防止了无效状态。这种统一的 User 枚举显著简化了 RelationTuple 的结构。它避免了需要单独的字段或复杂的条件逻辑来判断“用户”是个人还是组，从而使存储和评估关系元组的代码更清晰、更类型安全。在授权检查期间，可以利用 Rust 的模式匹配功能来处理不同的 User 变体，这直接反映了 Zanzibar 对主体设计的灵活性。

以下是其 Rust 表示：

Rust

/// 表示一个特定的用户 ID 或对用户集（例如，一个组）的引用。

#

pub enum User {
    UserId(String), // 示例: "10", "<alice@example.com>"
    Userset(Object, Relation), // 示例: Object { namespace: "group", id: "eng" }, Relation("member")
}

/// 表示特定命名空间内的数字对象。

#

pub struct Object {
    pub namespace: String, // 示例: "doc", "group", "folder"
    pub id: String,        // 示例: "readme", "eng", "A"
}

/// 表示对象上的关系或权限类型。

#

pub struct Relation(pub String); // 示例: Relation("owner"), Relation("editor"), Relation("viewer"), Relation("member"), Relation("parent")

/// 核心关系元组，表示单个权限断言。

#

pub struct RelationTuple {
    pub object: Object,
    pub relation: Relation,
    pub user: User,
}

在简化的内存存储上下文中，RelationTuple 结构本身，在正确派生 Hash 和 Eq 特性后，可以作为 HashSet<RelationTuple> 中的唯一键。

### **命名空间配置：定义策略模式**

在客户端能够存储或评估关系元组之前，它们必须定义其“命名空间”。命名空间配置指定了该命名空间中可以存在的关系（例如，viewer、editor），并且最重要的是，通过用户集重写规则来定义这些关系如何相互作用 1。此配置充当特定领域内访问控制的模式或策略定义。

其 Rust 表示如下：

Rust

/// 定义特定命名空间的模式和策略规则。
pub struct NamespaceConfig {
    pub name: String, // 示例: "doc"
    pub relations: HashMap<Relation, RelationConfig>, // 将关系名称映射到其配置
}

/// 定义命名空间内的特定关系，包括其重写规则。
pub struct RelationConfig {
    pub name: Relation, // 示例: Relation("owner"), Relation("editor")
    pub userset_rewrite: Option<UsersetExpression>, // 用于计算有效用户集的可选规则
}

### **用户集表达式树：策略逻辑引擎**

用户集重写规则是 Zanzibar 策略语言中最强大和灵活的方面。它们允许客户端定义复杂、与对象无关的权限关系，并启用继承，而无需为每个推断出的权限存储冗余的关系元组 1。这些规则以表达式树的形式组织。

论文确定了三种基本类型的叶节点 1：

* **this：** 这指的是通过存储的 RelationTuple 为正在评估的特定 object#relation 对直接指定的所有用户（或用户集）。它代表了显式、直接断言的权限。
* **computed_userset：** 这个原语通过引用**同一对象**上的另一个关系来计算一个新的用户集。例如，viewer 关系可能包括对**同一文档**拥有 editor 权限的所有用户，从而有效地创建权限继承。
* **tuple_to_userset：** 这是一个高度灵活的原语，允许表达像分层继承这样的复杂策略。它首先从输入对象计算一个 tupleset（例如，通过 parent 关系查找其父文件夹）。然后，对于在该元组集中找到的每个用户（必须是指向另一个 object#relation 的 Userset），它根据指定的关系（例如，从父文件夹继承 viewer 权限）计算一个用户集。这是 Zanzibar 用于分层继承的“指针追逐”机制。

这些叶子表达式可以使用标准的集合代数运算符进行组合：union（或）、intersection（与）和 exclusion（非）1。这允许定义高度细粒度和复杂的策略。

UsersetExpression 被描述为一种“表达式树”，由“叶节点”和“由多个子表达式组成，通过 union、intersection 和 exclusion 等操作组合”1。这种分层描述立即暗示了递归数据结构。在 Rust 中，一个枚举（enum）类型，其变体可以包含

Box<UsersetExpression>（用于单个子表达式，如 Exclusion）或 Vec<UsersetExpression>（用于多个子表达式，如 Union 或 Intersection），完美地建模了这种递归树结构。这种递归结构对于 Zanzibar 表达高度复杂和深度嵌套的访问策略的能力至关重要。例如，一个策略，如“文档的查看者也是其父文件夹的查看者，并且是文档的编辑者，但不是被明确排除的用户”，可以优雅地表示为单个 UsersetExpression 树。Rust 枚举及其递归变体直接将这种能力转化为类型安全和惯用的表示，这将是策略评估引擎（Check 和 Expand 函数）的骨干。

tuple_to_userset 原语被论文和相关材料明确强调为实现“查找文档的父文件夹并继承其查看者”等复杂策略的关键 1。这暗示了一个两步的逻辑过程：首先，通过查询特定关系（例如，当前对象的

parent 关系）来识别相关对象；其次，在**那些已识别的相关对象**上评估另一个关系（例如，viewer）。这是建模分层数据中继承的常见模式。在 UsersetExpression 枚举中明确包含 tuple_to_userset 至关重要，因为它直接支持对象级别的继承，这是实际访问控制系统（例如，文件系统、云资源）中的常见需求。没有这个原语，此类策略将需要手动去规范化（导致数据冗余和更新难题）或使用临时、不灵活的逻辑来实现。它的存在确保了简化的 Rust 实现捕获了 Zanzibar 策略表达的这一强大而实用的方面。

其 Rust 表示如下：

Rust

/// 表示用户集表达式树中的一个节点，定义了如何计算用户集。

#

pub enum UsersetExpression {
    /// 指的是当前 object#relation 的关系元组直接指定的用户。
    This,
    /// 根据同一对象上的另一个关系计算用户集。
    ComputedUserset {
        relation: Relation, // 例如，如果 viewer 包含 editor，则为 "editor"
    },
    /// 通过遍历一个关系来查找其他对象，然后评估这些对象上的一个关系来计算用户集。
    TupleToUserset {
        tupleset_relation: Relation, // 例如，对于 doc:readme#parent@folder:A，则为 "parent"
        computed_userset_relation: Relation, // 例如，对于 folder:A#viewer，则为 "viewer"
    },
    /// 表示多个子用户集表达式的并集（或）。
    Union(Vec<UsersetExpression>),
    /// 表示多个子用户集表达式的交集（与）。
    Intersection(Vec<UsersetExpression>),
    /// 表示从一个用户集中排除另一个用户集（非）。
    Exclusion {
        base: Box<UsersetExpression>,
        exclude: Box<UsersetExpression>,
    },
}

以下表格提供了 Rust 实现中核心数据类型的快速、整合参考。它直接回应了用户对详细数据结构的要求，使设计对于 Rust 开发人员而言立即变得可理解和可操作。它突出了 Zanzibar 抽象概念与 Rust 具体类型系统之间的直接映射。

**表 1：核心 Rust 数据结构**

| Rust 结构/枚举           | 用途                                   | 关键字段/变体                                                                                                                                      | 示例                                                                                     |
| :----------------------- | :------------------------------------- | :------------------------------------------------------------------------------------------------------------------------------------------------- | :--------------------------------------------------------------------------------------- |
| Object                   | 表示命名空间中的数字对象。             | namespace: String, id: String                                                                                                                      | Object { namespace: "doc", id: "readme" }                                                |
| Relation                 | 表示关系或权限的类型。                 | String (元组结构)                                                                                                                                  | Relation("owner"), Relation("member")                                                    |
| User (枚举)              | 表示主体：直接用户 ID 或用户集（组）。 | UserId(String), Userset(Object, Relation)                                                                                                          | User::UserId("10"), User::Userset(Object { ns: "group", id: "eng" }, Relation("member")) |
| RelationTuple            | 权限断言的原子单元。                   | object: Object, relation: Relation, user: User                                                                                                     | RelationTuple { object: doc:readme, relation: owner, user: User::UserId("10") }          |
| NamespaceConfig          | 定义命名空间的模式和策略规则。         | name: String, relations: HashMap<Relation, RelationConfig>                                                                                         | NamespaceConfig { name: "doc",... }                                                      |
| RelationConfig           | 定义命名空间内的特定关系。             | name: Relation, userset_rewrite: Option<UsersetExpression>                                                                                         | RelationConfig { name: "editor", userset_rewrite: Some(...) }                            |
| UsersetExpression (枚举) | 定义如何通过重写规则计算有效用户集。   | This, ComputedUserset { relation }, TupleToUserset { tupleset_relation, computed_userset_relation }, Union(...), Intersection(...), Exclusion(...) | UsersetExpression::Union(...)                                                            |

## **3. 核心 API 接口在 Rust 中的实现**

原始的 Zanzibar 系统依赖 Google 的 Spanner 来持久且一致地存储关系元组 1。对于这个简化的 Rust 实现，内存存储足以满足初始开发和测试的需求。然而，为了保持灵活性并允许将来升级到持久存储而不改变核心逻辑，一个抽象层是必不可少的。

### **TupleStore 特性：抽象数据存储**

TupleStore 特性充当一个契约，将授权评估逻辑（在 check 和 expand 函数中实现）与关系元组的存储和检索的具体细节解耦。这遵循了“依赖反转”原则，使系统更加模块化和可测试。

这个设计选择对于简化系统的长期适应性至关重要。如果将来需要引入持久性（例如，使用 SQLite、RocksDB 或简单的基于文件的存储），可以通过为新的后端实现 TupleStore 特性来完成。这允许无缝过渡，而无需完全重构核心授权逻辑，显著减少了未来的开发工作量并提高了系统的实用性。

TupleStore 定义了以下方法：

* read_tuples(&self, object: &Object, relation: Option<&Relation>, user: Option<&User>) -> Vec<RelationTuple>：此方法旨在查询底层存储中的关系元组。relation 和 user 的 Option 类型允许灵活查询，例如检索给定对象的所有关系，或特定对象-关系对的所有用户。它返回一个匹配的 RelationTuple 向量。
* write_tuple(&mut self, tuple: RelationTuple) -> Result<(), String>：此方法向存储中添加一个新的 RelationTuple。它返回一个 Result 以指示成功或潜在错误（例如，如果存储强制唯一性，则尝试写入重复元组）。
* delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), String>：此方法从存储中删除指定的 RelationTuple。它也返回一个 Result 以指示成功或错误（例如，如果未找到要删除的元组）。

对于初始简化版本，可以实现一个 InMemoryTupleStore 结构。此结构将在内部使用 HashSet<RelationTuple> 来存储元组。HashSet 凭借其哈希能力，提供了高效的插入、删除和查找（检查是否存在）功能，前提是 RelationTuple 及其组件（Object、Relation、User）正确地派生或实现了 Hash 和 Eq 特性。read_tuples 的查找将涉及遍历 HashSet 并根据提供的 object、relation 和 user 条件进行过滤。虽然这对于大型数据集未经优化，但对于简化的、非性能关键的模型来说是完全可接受的。

Rust

pub trait TupleStore {
    fn read_tuples(&self, object: &Object, relation: Option<&Relation>, user: Option<&User>) -> Vec<RelationTuple>;
    fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), String>;
    fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), String>;
}

pub struct InMemoryTupleStore {
    store: HashSet<RelationTuple>,
}

impl TupleStore for InMemoryTupleStore {
    //... read_tuples, write_tuple, delete_tuple 的实现细节...
}

### **Check 函数：授权决策点**

Check API 是客户端查询授权决策的主要接口：“用户 U 是否对对象 O 拥有关系 R？” 1。它返回一个布尔结果。

其函数签名如下：pub fn check(object: &Object, relation: &Relation, user: &User, config: &NamespaceConfig, store: &impl TupleStore) -> bool

check 函数的核心任务是根据 NamespaceConfig 和存储的 RelationTuples 评估有效权限。这涉及 UsersetExpression 树的递归遍历 1。

1. **检索关系配置：** 首先，函数从提供的 NamespaceConfig 中检索给定 object.namespace 和 relation 的 RelationConfig。
2. **确定用户集表达式：** 如果 RelationConfig 中定义了 userset_rewrite 规则，则使用该规则。如果未指定重写规则，则行为默认为 UsersetExpression::This，这意味着只考虑该 object#relation 的直接断言元组。
3. **递归遍历和评估：** UsersetExpression 的评估本质上是递归的，因为表达式可以包含其他表达式。
   * This：函数查询 store 以查找与 object#relation@putative_user 匹配的 RelationTuple。如果 putative_user 是 UserId，它会查找直接匹配。如果它是 Userset，它会针对该 Userset 递归调用 check。
   * ComputedUserset：函数针对**同一对象**上的 computed_userset.relation 递归调用 check。这处理了“编辑者也是查看者”等策略。
   * TupleToUserset：这涉及一个两步过程：
     * 首先，查询 store 以查找与 object#tupleset_relation@* 匹配的元组，以查找由 tupleset_relation 关联的所有对象。
     * 对于在这些元组中找到的每个 user（它必须是一个指向另一个 object#relation 的 Userset），针对**从找到的用户集中提取的对象**上的 computed_userset_relation 递归调用 check。这是 Zanzibar 用于分层继承的“指针追逐”机制 1。
   * Union、Intersection、Exclusion：这些运算符将其子表达式的布尔结果应用于标准集合逻辑。例如，如果**任何**子表达式评估为 true，则 Union 返回 true。
4. **基本情况：** 在应用所有重写规则和用户集扩展后，当在 store 中找到 putative_user 的直接 UserId 匹配时，递归终止。

原始的 Check API 包含一个 zookie 参数，以确保与客户端指定内容版本的一致性 1。本简化版 Rust 实现明确

**省略**了 zookie 及其相关的复杂一致性协议，仅在 TupleStore 的当前状态下运行。

### **Expand 函数：理解有效权限**

Expand API 与 Check 的输出不同。它不返回布尔决策，而是通过遵循所有通过用户集重写规则表达的间接引用，返回给定 (object#relation) 对的**有效用户集** 1。此 API 对于需要内省或推理谁有权访问的应用程序至关重要，例如为受访问控制的内容构建搜索索引或在 UI 中显示权限。

其函数签名如下：pub fn expand(object: &Object, relation: &Relation, config: &NamespaceConfig, store: &impl TupleStore) -> ExpandedUserset

论文指出，Expand 的结果是“一个用户集树，其叶节点是用户 ID 或指向其他 (object#relation) 对的用户集，中间节点表示并集、交集或排除操作符”1。这意味着一个结构，它反映了

UsersetExpression，但在叶子处是具体的 UserId，而不仅仅是 This 节点。

Rust

/// 表示 Expand 操作的结果，详细说明了有效用户集。

#

pub enum ExpandedUserset {
    User(String), // 拥有权限的具体用户 ID
    Userset(Object, Relation), // 对另一个贡献权限的用户集的引用
    Union(Vec<ExpandedUserset>), // 多个扩展用户集的并集
    Intersection(Vec<ExpandedUserset>), // 多个扩展用户集的交集
    Exclusion { // 从一个扩展用户集中排除另一个
        base: Box<ExpandedUserset>,
        exclude: Box<ExpandedUserset>,
    },
    Empty, // 表示空的用户集，用于基本情况或无权限
}

expand 函数的评估逻辑在概念上与 check 相似，因为它也递归遍历 UsersetExpression 树。然而，它不是返回布尔值，而是收集并组合在评估表达式树的叶子处找到的实际 UserId 和 Userset 引用。

* This：从 store 中收集与 object#relation 直接关联的所有 User。这些 User 然后被转换为 ExpandedUserset::User 或 ExpandedUserset::Userset 变体。
* ComputedUserset：针对**同一对象**上的 computed_userset.relation 递归调用 expand，并将结果合并到当前的 ExpandedUserset 中。
* TupleToUserset：类似于 check，它首先通过 tupleset_relation 找到相关对象。对于找到的每个 Userset，它针对**该用户集中的对象**上的 computed_userset_relation 递归调用 expand。然后将结果合并。
* Union、Intersection、Exclusion：这些运算符对其子表达式的 ExpandedUserset 结果应用集合操作（并集、交集、差集）。这可能涉及扁平化嵌套的 Union 或 Intersection 以获得更清晰的最终表示。

Check 和 Expand 函数虽然都依赖于相同的底层 UsersetExpression 评估逻辑，但它们的输出和对客户端应用程序的实用性却截然不同 1。这种差异表明它们是策略评估的两个方面，各自服务于独特而有价值的目的。设计和实现这两个函数，即使在简化模型中，也能确保 Rust 实现捕获 Zanzibar 核心功能的全部广度。

Check 对于应用程序关键路径中的实时授权决策至关重要。而 Expand 对于策略内省、审计以及启用高级功能（如构建受访问控制的搜索索引或向用户显示全面的权限视图）至关重要。包含两者使简化系统更加完整和通用。

### **递归控制：确保稳定性**

论文指出，评估组成员关系可能“需要遵循一长串嵌套的组成员关系”1，并将评估过程描述为“递归指针追逐”1。尽管简化模型在深度递归的性能优化方面做出了妥协，但不受控制的递归可能导致关键问题，例如堆栈溢出错误或无限循环，如果策略定义中存在循环依赖（例如，组 A 是组 B 的成员，组 B 又是组 A 的成员）。

因此，即使在简化的单节点系统中，Check 和 Expand 的实现也必须包含管理递归的机制。这可以涉及传递一个 recursion_depth 计数器，并在超过预定义限制时返回错误或 false。更健壮的方法是维护一个当前正在评估的 (object, relation, user) 元组的 HashSet，以检测并打破循环，防止无限循环。虽然在论文中并未明确提及为“简化”要求，但对于任何健壮的递归算法，尤其是在安全敏感的环境中，这都是一个至关重要的正确性和稳定性考虑。

以下表格提供了与 Zanzibar 启发的系统交互的主要接口的清晰、简洁概述。它通过提供它们的签名和目的的简要解释，直接回应了用户对“核心 API 接口”的要求，使开发人员能够轻松理解如何使用该系统。

**表 2：建议的 Rust API 函数/特性**

| API 组件           | 类型   | 签名                                                                                                                                                                                                                                                | 用途                                                                            |
| :----------------- | :----- | :-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | :------------------------------------------------------------------------------ |
| TupleStore         | 特性   | read_tuples(&self, object: &Object, relation: Option<&Relation>, user: Option<&User>) -> Vec<RelationTuple> write_tuple(&mut self, tuple: RelationTuple) -> Result<(), String> delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), String> | 存储和检索 RelationTuple 的抽象接口，允许灵活的后端实现。                       |
| InMemoryTupleStore | 结构体 | 实现 TupleStore                                                                                                                                                                                                                                     | TupleStore 的具体内存实现，用于简化和快速原型开发。                             |
| check              | 函数   | fn check(object: &Object, relation: &Relation, user: &User, config: &NamespaceConfig, store: &impl TupleStore) -> bool                                                                                                                              | 评估给定 user 是否对 object 拥有特定 relation，返回布尔决策。                   |
| expand             | 函数   | fn expand(object: &Object, relation: &Relation, config: &NamespaceConfig, store: &impl TupleStore) -> ExpandedUserset                                                                                                                               | 返回对 object 拥有特定 relation 的有效用户和用户集，表示为 ExpandedUserset 树。 |

## **4. DSL 兼容层设计**

DSL 语法的设计大量借鉴了 Zanzibar 论文中提供的关系元组的文本表示和结构化配置示例，特别是图 1 1。目标是创建一种人类可读写的格式，直接映射到第 2 节中定义的 Rust 数据结构。

### **DSL 语法：策略的文本表示**

DSL 将表示命名空间定义的集合，通常包含在单个策略文件或字符串中。每个 namespace 块将定义其唯一名称并包含一个或多个 relation 定义。每个 relation 块将指定关系的名称，以及一个可选的 rewrite 规则。如果未明确提供 rewrite 规则，它将隐式默认为 this 行为（即，直接元组查找）。rewrite 规则将使用类似函数的语法来表示 UsersetExpression 原语和集合运算符。

DSL 作为 UsersetExpression 结构的直接、人类可读的反映，与 UsersetExpression 枚举的设计紧密相连。论文中的示例（图 1 1）和详细解释 1 清晰地说明了重写规则的嵌套、类似函数的结构。这种文本 DSL 与 Rust 枚举之间的直接结构对应关系并非偶然；它是表示 Zanzibar 策略逻辑最直观的方式。一个精心设计的 DSL 不仅仅是任意的语法；它是底层数据模型的清晰、人类可读的映射。通过使 DSL 语法直接反映

UsersetExpression 枚举的变体及其嵌套功能，解析过程变得更加简单。更重要的是，对于理解 Zanzibar 核心概念的用户来说，策略定义变得直观，从而提高了策略编写的可用性并减少了错误。这确保了 DSL 不仅“兼容”，而且对于 Zanzibar 强大的模型来说也是“惯用的”。

示例 DSL（受图 1 启发 1）：

// 定义用于文档的“doc”命名空间
namespace doc {
    // “owner”关系，默认为直接元组
    relation owner {}

    // “editor”关系：包括直接编辑者 或 拥有者
    relation editor {
        rewrite union(
            this, // doc#editor 的直接元组
            computed_userset(relation: "owner") // 拥有此文档的用户
        )
    }

    // “viewer”关系：包括直接查看者 或 编辑者 或 父文件夹的查看者
    relation viewer {
        rewrite union(
            this, // doc#viewer 的直接元组
            computed_userset(relation: "editor"), // 拥有此文档的编辑者
            tuple_to_userset( // 父文件夹的查看者
                tupleset: "parent", // 通过 doc#parent@folder:A 查找父文件夹
                computed_userset: "viewer" // 然后检查这些父文件夹的 viewer 关系
            )
        )
    }
}

// 定义用于用户组的“group”命名空间
namespace group {
    // “member”关系，默认为直接元组
    relation member {}
}

DSL 语法的关键元素包括：

* **关键字：** namespace、relation、rewrite、union、intersection、exclusion、this、computed_userset、tuple_to_userset。
* **标识符：** 用户定义的命名空间、关系的名称（例如，doc、owner、editor、viewer、group、member、parent）。
* **结构：** 花括号 {} 用于定义块（命名空间、关系），圆括号 () 用于类似函数的调用（例如，union(...)、computed_userset(...)），逗号 , 用于分隔函数调用中的参数。
* **命名参数：** relation: "name"、tupleset: "name"、computed_userset: "name" 用于清晰度，模仿 Rust 结构体字段。

### **解析策略：将文本转换为 Rust 结构**

将文本 DSL 转换为 Rust 数据结构（NamespaceConfig、RelationConfig、UsersetExpression）的过程涉及几个标准的编译器/解析器阶段：

* **词法分析（Tokenization）：** 输入的 DSL 字符串逐字符处理，生成有意义的“词元”流。例如，字符串 namespace doc { 将被分解为 NAMESPACE、IDENTIFIER("doc")、LBRACE 等词元。
* **解析（语法分析）：** 词元流根据预定义的语法进行分析，以构建抽象语法树（AST）。AST 是程序结构的分层表示，反映了命名空间、关系和用户集表达式的嵌套。此阶段检查语法正确性。
* **语义分析/转换：** 遍历 AST 以执行语义检查（例如，确保引用的关系存在），最重要的是，将 AST 节点转换为具体的 Rust 数据结构实例（NamespaceConfig、RelationConfig、UsersetExpression）。这是 DSL 的抽象表示成为系统可执行策略模型的地方。

虽然请求允许采用高级解析策略，但“DSL 兼容性”功能的长期成功和可靠性取决于精确且明确的语法。DSL 语法中的任何歧义都将不可避免地导致复杂、易出错的解析逻辑和策略加载时的不可预测行为。Zanzibar 配置的结构化性质（例如，论文中的 union { child {... } child {... } }）强烈表明它可以用形式语法（例如，上下文无关文法）来描述。即使是对于一个简化的实现，定义概念性 DSL 语法后的一个关键后续步骤将是将其语法形式化。这可能涉及使用扩展巴科斯-诺尔范式（EBNF）或类似的表示法。形式语法为解析器提供了明确的规范，确保它正确解释所有有效策略并优雅地处理无效策略。这一基础步骤对于构建可靠且可预测的 DSL 解析器至关重要，而这对于任何依赖外部配置作为其核心逻辑的系统都至关重要。

对于相对简单的 DSL，可以直接在 Rust 中实现**手动递归下降解析器**。这种方法涉及为每个语法规则编写函数，这些函数递归调用其他函数来解析子规则。对于更健壮或复杂的 DSL，Rust 提供了强大的**解析器组合器库**（如 nom）或**解析器生成器框架**（如 pest，它使用 PEG 语法）。

### **将 DSL 映射到 Rust 结构：示例**

这些示例展示了所提议 DSL 片段与其对应的 Rust 数据结构表示之间的直接一对一映射。这种清晰度对于 DSL 兼容性至关重要。

**表 3：DSL 到 Rust 映射示例**

| DSL 片段                                                         | 对应的 Rust 结构 (UsersetExpression 或 RelationConfig)                                                                                                            |
| :--------------------------------------------------------------- | :---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| relation owner {}                                                | RelationConfig { name: Relation("owner"), userset_rewrite: Some(UsersetExpression::This) }                                                                        |
| computed_userset(relation: "editor")                             | UsersetExpression::ComputedUserset { relation: Relation("editor") }                                                                                               |
| tuple_to_userset(tupleset: "parent", computed_userset: "viewer") | UsersetExpression::TupleToUserset { tupleset_relation: Relation("parent"), computed_userset_relation: Relation("viewer") }                                        |
| union(this, computed_userset(relation: "owner"))                 | UsersetExpression::Union(vec!)                                                                                                                                    |
| intersection(this, computed_userset(relation: "admin"))          | UsersetExpression::Intersection(vec!)                                                                                                                             |
| exclusion(this, computed_userset(relation: "blocked"))           | UsersetExpression::Exclusion { base: Box::new(UsersetExpression::This), exclude: Box::new(UsersetExpression::ComputedUserset { relation: Relation("blocked") }) } |

## **5. 实现路径与考量**

本节概述了在 Rust 中构建简化版 Zanzibar 系统的分阶段方法。这种增量策略有助于管理复杂性，确保每个步骤的稳定性，并为开发提供清晰的里程碑。

### **阶段 1：基础——数据结构和基本存储**

此阶段的重点是构建系统的核心数据模型和基本的内存存储机制。

* **任务：**
  * **定义核心结构体和枚举：** 实现第 2 节中设计的 Object、Relation 和 User 枚举/结构体。确保 Object、Relation 和 User（以及随后的 RelationTuple）派生或手动实现 Debug、PartialEq、Eq、Hash 和 Clone 特性。这些对于在 HashSet 和 HashMap 中有效使用它们以及进行调试至关重要。
  * **定义 TupleStore 特性：** 实现 TupleStore 特性及其 read_tuples、write_tuple 和 delete_tuple 方法。
  * **实现 InMemoryTupleStore：** 创建 InMemoryTupleStore 结构体，它将在内部持有 HashSet<RelationTuple>。为该结构体实现 TupleStore 特性。read_tuples 方法将涉及遍历 HashSet 并根据提供的 object、relation 和 user 条件进行过滤。
* **验证：**
  * 为 InMemoryTupleStore 编写单元测试，以确认添加、删除和检索特定元组的正确行为。测试边界情况，例如添加重复元组（如果强制唯一性）或删除不存在的元组。
  * 确保核心数据类型的 Hash 和 Eq 实现行为符合预期，特别是对于具有嵌套结构的 User 枚举。

### **阶段 2：基本检查逻辑——直接元组**

此阶段引入了授权检查的核心功能，但最初仅限于直接的关系元组查找。

* **任务：**
  * **初步 check 函数：** 实现 check 函数的初始版本。在此阶段，它应仅处理 InMemoryTupleStore 中的直接 RelationTuple 查找。这意味着它有效地模拟了 UsersetExpression::This 的行为。
  * **基本 NamespaceConfig：** 定义 NamespaceConfig 和 RelationConfig 结构体，但为了简单起见，UsersetExpression 部分可以暂时忽略或用占位符表示（例如，Option<bool> 表示关系是否简单存在）。此处的目的是建立函数签名和基本数据流。
* **验证：**
  * 使用简单场景为 check 函数编写单元测试。例如，验证 check(doc:readme, owner, alice) 仅在 doc:readme#owner@alice 直接存在于 InMemoryTupleStore 中时才返回 true。
  * 测试用户没有直接权限的情况。

### **阶段 3：用户集重写评估——核心逻辑**

这是实现 Zanzibar 策略评估灵活性的最关键阶段，涉及递归地处理用户集重写规则。

* **任务：**
  * **实现 UsersetExpression：** 完整定义 UsersetExpression 枚举及其所有变体：This、ComputedUserset、TupleToUserset、Union、Intersection 和 Exclusion。
  * **完善 RelationConfig：** 更新 RelationConfig 结构体，以正确包含 Option<UsersetExpression>，允许策略指定重写规则。
  * **实现完整的递归评估：** 增强 check 函数以递归评估 UsersetExpression 树。这是实现中最复杂的部分。
    * 通过查询 TupleStore 来处理 This。
    * 通过对**同一对象**但使用不同 relation 进行递归调用 check 来实现 ComputedUserset。
    * 实现 TupleToUserset：首先通过查询 TupleStore 查找中间对象（例如，通过 tupleset_relation 找到父文件夹）。然后，对于每个找到的中间对象，递归调用 check 以评估其 computed_userset_relation。
    * 实现 Union、Intersection 和 Exclusion 逻辑，它们将对其子表达式的递归评估结果应用集合操作。
  * **实现递归深度限制/循环检测：** 为了防止无限循环或堆栈溢出，在 check 函数中集成递归深度限制或使用 HashSet 跟踪当前调用堆栈中的 (object, relation, user) 对以检测循环。
* **验证：**
  * 为每个 UsersetExpression 变体编写详细的单元测试。
  * 创建包含嵌套 computed_userset 和 tuple_to_userset 规则的复杂策略，并验证 check 函数的正确性。
  * 测试循环依赖关系和递归深度限制，确保系统在检测到此类情况时能够优雅地处理。

### **阶段 4：Expand API 实现**

此阶段专注于实现 Expand API，它提供了对有效权限的内省能力。

* **任务：**
  * **定义 ExpandedUserset：** 实现 ExpandedUserset 枚举，它将表示 Expand 操作的输出。
  * **实现 expand 函数：** 编写 expand 函数，其逻辑与 check 类似，但它收集并组合实际的用户 ID 和用户集引用，而不是返回布尔值。它还将递归遍历 UsersetExpression 树。
  * **处理集合操作：** 确保 expand 函数中的 Union、Intersection 和 Exclusion 逻辑正确地操作 ExpandedUserset 结果，可能需要扁平化或简化结果树。
* **验证：**
  * 为 expand 函数编写单元测试，验证它为各种策略（包括嵌套规则）返回正确的 ExpandedUserset 结构。
  * 测试 expand 如何处理空权限集和复杂集合操作。

### **阶段 5：DSL 解析器开发**

此阶段将 DSL 文本转换为可执行的 Rust 策略模型。

* **任务：**
  * **设计 DSL 词法分析器：** 创建一个将 DSL 字符串转换为词元流的组件。
  * **设计 DSL 解析器：** 实现一个解析器，它将词元流转换为 NamespaceConfig 和 RelationConfig 结构体，其中包含 UsersetExpression。这可能涉及手动递归下降解析器或使用像 nom 或 pest 这样的库。
  * **实现 DSL 到 Rust 结构的映射：** 确保解析器正确地将 DSL 语法元素映射到第 2 节中定义的 Rust 数据结构。
* **验证：**
  * 为词法分析器和解析器编写单元测试，以确保它们正确处理有效和无效的 DSL 输入。
  * 测试解析器是否能正确地将示例 DSL 策略转换为预期的 Rust NamespaceConfig 实例。

### **阶段 6：集成和测试**

最后一个阶段涉及将所有组件集成在一起，并进行全面的端到端测试。

* **任务：**
  * **集成所有组件：** 将 TupleStore、check、expand 函数与 DSL 解析器集成。
  * **创建示例应用程序：** 开发一个简单的命令行界面或测试套件，允许加载 DSL 策略，添加/删除关系元组，并执行 check 和 expand 操作。
  * **性能基准测试（简单）：** 对于简化的内存模型，进行基本的性能测试，以了解在不同数量的元组和策略复杂性下的响应时间。
* **验证：**
  * 执行端到端测试，模拟客户端服务如何使用 Zanzibar。
  * 进行压力测试，以识别潜在的瓶颈或内存问题（在内存模型中）。
  * 确保所有错误处理路径都经过测试。

### **额外考量**

* **错误处理：** 在整个实现过程中，应仔细考虑错误处理。例如，TupleStore 的写入/删除操作应返回 Result，解析器应报告语法错误，并且评估函数应处理配置查找失败等情况。
* **日志记录：** 集成日志记录框架（例如 log crate），以便在开发和运行时跟踪系统行为和调试问题。
* **文档：** 随着开发的进行，为代码和 API 提供清晰的文档，以帮助未来的维护和扩展。

## **6. 结论**

本报告提供了一个简化版 Rust 实现的详细设计蓝图，该实现基于 Google Zanzibar 授权系统的核心概念。通过策略性地规避 Zanzibar 分布式和性能方面的复杂性，本设计将重点放在忠实地表示其独特的数据模型和强大的策略语言上。

本报告中提出的核心数据结构，特别是 RelationTuple、NamespaceConfig 和递归的 UsersetExpression 枚举，捕获了 Zanzibar 表达复杂访问控制策略的灵活性。User 枚举的引入统一了主体表示，简化了逻辑，而 UsersetExpression 的递归性质则支持了深度嵌套的策略定义。tuple_to_userset 原语的明确包含，确保了对对象层次结构和继承这一常见授权模式的直接支持。

TupleStore 特性的设计是一个关键的架构决策，它将核心授权逻辑与底层存储机制解耦。这种抽象使得初始实现可以使用简单的内存存储，同时为将来无缝集成更持久的本地存储解决方案（如 SQLite）奠定了基础，无需大规模重构。Check 和 Expand 函数作为核心 API 接口，提供了 Zanzibar 授权系统的两个基本视图：实时决策和策略内省，满足了不同的客户端需求。同时，对递归控制的关注确保了即使在简化模型中，系统的稳定性和正确性。

此外，本报告提出了一个受 Zanzibar 原始配置启发而设计的领域特定语言（DSL）。该 DSL 旨在直接映射到 Rust 数据结构，提供一种人类可读、可编写的策略定义方式。通过将 DSL 语法与 UsersetExpression 枚举的结构紧密对齐，解析过程变得更加直观，并且策略定义对于理解 Zanzibar 核心概念的用户来说也更易于理解。

所概述的分阶段实现路径提供了一个结构化的方法，从构建基础数据结构和存储开始，逐步推进到核心策略评估逻辑、API 实现和 DSL 解析器开发。这种增量方法有助于管理复杂性，确保每个阶段的稳定性，并为开发团队提供清晰的里程碑。

总之，本简化版 Rust Zanzibar 实现提供了一个坚实的基础，用于理解和实验 Zanzibar 的强大授权模型。它为开发人员提供了一个可操作的蓝图，可以构建一个功能齐全的本地授权服务，该服务能够处理复杂的访问控制策略，并为未来的扩展和集成留有余地。
