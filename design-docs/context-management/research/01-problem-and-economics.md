# Problem and Economics

## Agentic Context Management: Solving Agent Memory and Cost by Treating Them as Lifecycle and Architecture Problems
URL: https://arxiv.org/html/2607.21503 (2026)
- Frames context as a lifecycle (architect, ingest, scope, anticipate, compact), not a store.
- Cost model: naive full-append grows token cost quadratically in conversation length. Crude summarization buys linear cost at an accuracy cliff. Validated compaction achieves linear cost with checked fidelity.
- Documented cliff case (citing Zhang et al. 2025): single-step compression 18,282 -> 122 tokens dropped accuracy 66.7% -> 57.1%, below the no-context baseline.
- Compaction cadence math: holding context near budget W with compaction every p turns at cost c*W gives total N*W*(1+c/p); at t=500 tok/turn, W=4000, p=8, c=2 (1.25x overhead), savings ~80% at 100 turns, 90% at 200, 96% at 500.
- Verifiable compaction: each pass checked for information loss, explicit validation score + compression ratio, automatic retry at lower aggressiveness. Iterated compaction is only safe because each pass is validated.
- Design implication: the architecture needs a validation gate on every compaction, and compaction must run on (compacted state + recent turns), never re-derive from full raw history.

## Context Compaction Theory
URL: https://arxiv.org/abs/2608.01326 (2026-08-02)
- Formalizes compaction as two games: Context Selection (retain a subset) vs Context Generation (emit bounded summary). Generation is equivalent to one-way communication complexity; selection is a restricted protocol class.
- Theorem: families of queries exist where generation needs Theta(log n) less budget than selection at equal error. Summarization provably dominates truncation.
- Case study: Anthropic's context compaction endpoint answers set-membership queries with substantially higher error than a Bloom filter of the same size. Production compaction is far from optimal.
- Classifies deployed agents (Codex, Claude Code, Gemini CLI, OpenCode, Goose, Cursor): all carry the compacted context forward rather than re-deriving from stored raw history each turn.
- Carry-forward cost argument (worked example, Claude Fable 5 pricing): re-deriving a 100K summary from 800K history costs ~$13 and ~26 minutes of output time per query; prompt caching softens input cost but not output cost.
- Design implication: carry compacted state forward as the new base; never re-derive per turn; prefer generation (structured summaries) over selection (truncation) at equal budget.

## What Does Context Compression Cost an Agent? Interaction Costs Unrevealed by Task-Completion Metrics
URL: https://arxiv.org/html/2608.16370v1 (2026-08)
- Controlled protocol, 24-turn budget, three models (DeepSeek v4-flash, Qwen3.7-plus, GPT-5.5).
- After compression, retrieval tool calls increase in all six model-regime cells (significant in 5/6 after Holm correction); execution calls unchanged. GPT-5.5: completion statistically unchanged (80->85%, p=1.0) while retrieval roughly tripled (+42.9 calls).
- Completion metrics miss this cost; the reacquisition signal responds at milder compression than completion does.
- Oracle restoration of dropped state removes ~half the retrieval cost. A fact-preserving operator at the same ratio preserves completion while avoiding most extra retrieval.
- Retention findings: history-dependent state (compact) should be retained fully at any budget; within the rest, recency is a competitive approximation under tight budgets.
- Design implication: measure interaction cost (tool-call inflation) post-compaction, not just success; preserve execution-relevant state (file paths, test names, error strings); give the agent a cheap read-back path so reacquisition costs one tool call, not a re-exploration.

## Lost in the Middle (Liu et al., TACL 2024; via LongLLMLingua)
URL: https://aclanthology.org/2024.acl-long.91/
- Position bias: models use information at the beginning and end of long contexts reliably; middle placement degrades recall.
- Design implication: preserve head (task, constraints) and tail (recent rounds) verbatim; compress the middle. Any compaction layout should keep invariant content at the head and the working set at the tail.

## A Survey of Context Engineering for Large Language Models
URL: https://arxiv.org/html/2507.13334 (2025-07; 1300+ papers)
- Taxonomy: context retrieval/generation, context processing, context management (memory hierarchies, compression, optimization).
- Notes quadratic attention cost, repeated-context processing costs in commercial deployment, reliability decay (hallucination, unfaithfulness) with long inputs.
- Design implication: positions our work inside "context management"; confirms the field treats this as a systems discipline, not prompt tuning.
