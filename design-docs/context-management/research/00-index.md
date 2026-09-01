# Context Management Research Corpus

Collected 2026-08-31 for the llxprt-code long-horizon context-management architecture.
Scope: compression, summarization, and context management for coding/terminal agents.
Each themed file contains per-paper digests: URL, findings, numbers, design implications.

## Files

- `01-problem-and-economics.md` — why append-only fails: cost models, formal bounds, hidden interaction costs, position effects
- `02-compaction-failure-modes.md` — what breaks when you compress: the Compaction Cliff, instability, taxonomy of failures
- `03-active-and-typed-management.md` — the winning family: typed retention, folding, agent-initiated management, hierarchical memory
- `04-terminal-and-code-specific.md` — terminal/coding evidence: TACO, benchmarks (Terminal-Bench 2.1/3.0/4.0, LHTB, SWE-Bench)
- `05-production-practice.md` — what deployed systems do: Anthropic compaction/context-editing/memory APIs, Claude Code, Codex-class agents
- `06-token-pruning-and-discounted.md` — the token-pruning family and why it is discounted for this design

## Headline conclusions

1. Append-only replay is the O(N^2) baseline; validated compaction is the only regime with linear cost and preserved fidelity (lifecycle paper). Days-long tasks make this non-optional.
2. Uniform summarization is the documented failure mode: production /compact preserves 53% of safety rules after one round, 10% after five (Compaction Cliff). Typed retention lanes fix this.
3. Folding/branching (Context-Folding, AgentFold) beats summarization baselines on SWE-Bench Verified with ~10x smaller active context. Structure-aware compression at subtask boundaries is the strongest single result.
4. Compression before entry beats compression after entry for tool outputs: TACO shows terminal observations should be filtered by preservation-aware rules before they ever join history.
5. Lossless offload + read-back is required, not optional: agents silently re-acquire dropped state via retrieval storms unless given a cheap read path (interaction-costs paper, ACM).
6. Cache economics are a first-class constraint: every prefix rewrite (clearing, compaction) invalidates the prompt cache; batch rewrites and make them worth the re-write (Anthropic clear_at_least).
7. Benchmarks: Terminal-Bench 4.0 (8-hour tasks, best 51.82%), LHTB (9.9M tokens/task, best 15-28% at R>=0.95) both name long-horizon context management as the bottleneck. Headroom is real.
