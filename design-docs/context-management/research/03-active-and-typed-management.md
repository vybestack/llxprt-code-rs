# Active and Typed Context Management (the winning family)

## ACM: Agentic Context Management for Long Horizon Tasks
URL: https://arxiv.org/abs/2607.23809 (2026; code/data open at github.com/lixiaochuan2020/agentic-context-management)
- Two purpose-built tools: manage_context (compress all messages up to the previous summary boundary; offload raw messages to external storage) and query_memory (querier LLM answers from the raw offloaded messages).
- Two properties: lossless (raws preserved externally, revisitable any time) and agent-initiated (compression at any point before peak, following evolving reasoning focus, vs fixed schedules or external triggers).
- Post-training pipeline with dual constraints (when to invoke AND when to refrain) because current models cannot decide when to compress unaided.
- Beats ReAct and summary-agent baselines on agentic search and coding benchmarks; benefits traced to reduced peak token pressure, longer effective exploration, more consistent solutions across trials.
- Design implication: expose compaction and read-back as model-visible operations; offload raws (we already persist rounds in the session store); schedule alone underperforms.

## Cat: Context as a Tool — Context Management for Long-Horizon SWE-Agents
URL: https://aclanthology.org/2026.findings-acl.1032/ (ACL 2026 Findings)
- Coding-agent-specific. Structured context workspace: stable task semantics, condensed long-term memory, high-fidelity short-term interactions.
- Context maintenance elevated to a callable tool in the agent's decision process; compression at appropriate milestones via trajectory-level supervision (CaT-Generator injects context-management actions into complete trajectories).
- SWE-Compressor: 57.6% on SWE-Bench-Verified, outperforming ReAct-based agents and static compression baselines under a bounded context budget.
- Design implication: for SWE agents, a three-tier workspace (task semantics / condensed history / verbatim recent) with milestone-triggered compression is the empirically validated layout.

## Scaling Long-Horizon LLM Agent via Context-Folding (FoldAgent)
URL: https://arxiv.org/abs/2510.11967 (ICML 2026; github.com/sunnweiwei/FoldAgent)
- Branch/return mechanism: agent branches into a temporary sub-trajectory for a localized subtask, returns a summary to the main thread, intermediate steps are folded away.
- 58.0% on SWE-Bench Verified and 62.0% on BrowseComp-Plus with a 32K active token budget (max 10 branches), surpassing baselines requiring 327K context; significantly outperforms summarization-based context management at equal budget.
- FoldGRPO process rewards (training-side, informative as policy targets): unfolded-token penalty when main thread exceeds 50% of budget (pushes token-heavy ops into branches); out-of-scope penalty for branch drift; failure penalty for failed tool calls.
- Authors note summary-based post-hoc compression abruptly disrupts working context and reasoning flow; folding preserves short-term context while managing long-term.
- Design implication: task/subtask structure is the right compression boundary; the harness can enforce branch isolation for token-heavy exploration (codebase greps, doc reads) without RL by making branches a first-class harness construct.

## AgentFold: Long-Horizon Agents with Proactive Context Folding
URL: https://arxiv.org/abs/2510.24699 (ICLR 2026)
- Folding at multiple scales: granular condensation (single step, fine details reserved) and deep consolidation (whole subtask chains coarsely summarized once complete).
- Context stays ~7K tokens after 100 turns; scales to 500 turns; context is non-monotonic: abandoning a failed line of inquiry resets context to a compact state (self-correction as context management).
- 30B-A3B model: 36.2% BrowseComp, 47.3% BrowseComp-ZH, surpassing much larger open models and o4-mini.
- Design implication: two granularities (step-level and subtask-level), and dead-end abandonment as a context-reset event; context need not grow monotonically within a task.

## HyMem: Hierarchical Context Management via Information Isolation
URL: https://arxiv.org/html/2608.15703 (2026-08)
- Root-cause argument: compression/retrieval operate on a flat pre-mixed trajectory where planning signals and execution noise are already blended; the fix is typed isolation at the source.
- Planner context never receives raw execution traces; only schema-constrained returns and structured summaries cross the boundary. Isolated reasoning module handles complex subtasks without polluting persistent planning context. Training-free.
- DeepSeek-V4: 66.7% GAIA, 61.3% Browsecomp-plus (+6.1/+4.7 pp over strongest baseline).
- Design implication: the cheapest byte to compress is the one that never enters the decision context; separate the planner-facing transcript from execution traces architecturally, not just at compaction time.

## Weighted Memory Tree (WMT)
URL: https://arxiv.org/html/2608.20631v1 (2026-08)
- Hierarchical memory (task/subtask/action) with dynamic retention scores; event-based updates and selection-based decay; completed branches folded, low-utility content suppressed, folded context still reachable.
- GAIA-Text: +9.97 pp accuracy over linear memory with 32.8% fewer prompt tokens; memory-poisoning experiments show reduced attack persistence/propagation.
- Design implication: retention is a scored lifecycle (promote on use, decay on disuse), not a one-shot decision at compaction time; utility scoring also bounds stale/poisoned content.
