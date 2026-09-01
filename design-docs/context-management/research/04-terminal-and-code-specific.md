# Terminal- and Code-Specific Evidence

## TACO: Self-Evolving Terminal Agent Compression via Observational Context Compression
URL: https://arxiv.org/abs/2604.19572 (2026-04; github.com/multimodal-art-projection/TACO)
- Terminal observations are not ordinary long text: heterogeneous, low-information-density execution traces with sparse exact evidence (error messages, file paths, test names, versions) interleaved with redundant output.
- Generic LLM summarization paraphrases or blurs exact strings later actions depend on; static heuristics are brittle across commands/repos; trained pruners (SWE-Pruner, LongCodeZip) need training data and target SWE workflows only.
- TACO compresses BEFORE observations enter history: LLM proposes structured rules (trigger pattern + compression action), conservative executor applies only on trigger match, preservation-aware by design (exact evidence preserved); rules evolve within a task (implicit feedback: requests for full output, repeated commands = over-compression) and accumulate in a Global Rule Pool across tasks.
- Results: +1-4% absolute accuracy on TerminalBench, +2-3% under matched token budgets; 12-27% total token reduction across TB 1.0/2.0, SWE-Bench Lite, CompileBench, DevEval, CRUST Bench, across models and scaffolds.
- Design implication: an observation-side filter with preservation guarantees (exact strings survive, bulk noise does not) improves BOTH accuracy and cost; pre-entry beats post-entry for tool output; rules can start hand-written per tool (ls, grep, build logs) and evolve.

## Long-Horizon-Terminal-Bench (LHTB)
URL: https://arxiv.org/html/2607.08964 (2026-07)
- 46 long-horizon terminal tasks (experiment reproduction, SWE, multimodal analysis, games, scientific computing), dense subtask-level grading with partial credit.
- 15-17 frontier agents: average 9.8-9.9M tokens per task, ~231-239 episodes, ~85-89 minutes per run. Best config: 15.2% (GPT-5.5) to 28.3% (Grok 4.5) pass@1 at R>=0.95; 10 of 15 models pass zero tasks at R>=1.0.
- Paper's own diagnosis: the bottleneck is not local reasoning but reliable long-horizon completion; agents time out after partial progress; voluntary early exits reflect weak self-verification; calls for stronger memory and progress tracking.
- Design implication: days-long tasks are exactly this regime (LHTB caps at 90 min and already breaks); progress tracking and calibrated stopping are context-management problems: the working context must carry verified state, not just history.

## Terminal-Bench leaderboard snapshots (2026)
URLs: https://snorkel.ai/leaderboard/terminal-bench-2-1/ , https://snorkel.ai/leaderboard/terminal-bench-3-0/ , https://benchlm.ai/benchmarks/terminal-bench-4
- TB 2.1 (89 tasks, fixed): Claude Code + Claude 5 Fable 83.8%, Codex CLI + GPT-5.5 83.1%, Terminus 2 + Fable 80.4%.
- TB 3.0 (66 harder tasks, adversarially reviewed, rolling): Claude Opus 5 + mini-SWE-agent 42.7% (7.3B tokens, $5.8k per full run), GPT-5.6 Sol + Codex 34.6%, Claude Fable 5 + Claude Code 34.1% (3.6B tokens).
- TB 4.0 (Aug 2026 snapshot; 66 tasks, 5 trials, 8-HOUR timeout): Claude Opus 5 + Claude Code 51.82%, Claude Fable 5 44.55%, GLM-5.3 + Claude Code 41.82%.
- Companion senior-engineering benchmark (SWE-Pro class): top ~34.7% tasteful solve rate.
- Design implication: the target regime is 8-hour, multi-billion-token runs where the best systems complete half the tasks; harness-level context management is a differentiator the leaderboard cannot attribute to the model.

## SWE results from research agents (cross-reference)
- Context-Folding: 58.0% SWE-Bench Verified at 32K active tokens (file 03).
- Cat/SWE-Compressor: 57.6% SWE-Bench-Verified under bounded budget (file 03).
- Both beat or match agents using ~10x the context, with structure-aware compression rather than raw window size.
