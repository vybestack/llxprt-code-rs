# Compaction Failure Modes

## The Compaction Cliff in Long-Running AI Agent Memory (Knowledge Triage)
URL: https://arxiv.org/html/2608.22752 (CIKM 2026)
- Headline measurement: on 20 production agent configurations, Claude Code's /compact prompt on Sonnet 4.6 preserves 53% of safety rules after one compaction round, 10% after five.
- Root cause: type-blind compaction. A safety rule and an episodic log compete for tokens and are summarized at the same rate, but only the rule needs exact wording to stay enforceable.
- Fix: Knowledge Triage. Classify each knowledge item by type; route each type through its own retention policy. Five knowledge types cover 97% of real configuration content (corpus: 396,934 agent configs from 54,628 GitHub repos).
- Three operators: TypeCompact (rewrite in place under per-type fidelity; preserves 2-4x more safety rules than the best single-shot LLM compactor at every ratio; 96% recall over five rounds), TypeDecompose (partition too-large topics, replicate scoped rules across partitions; 0% locality violations vs 93% under uniform partitioning), TypeRetrieve (fetch from external storage with in-scope rules pinned ahead of relevance; 100% recall@50 vs 73% for the best single-shot LLM retriever, zero LLM tokens per query).
- SafetyMargin classifier scores by counterfactual safety rather than grammatical form.
- Design implication: typed retention lanes are the core fix; constraints/decisions get verbatim (or formally checked) preservation, logs get aggressive compression; when partitioning context, replicate global rules into each partition; when retrieving, pin rules ahead of relevance-ranked items.

## Toward Reliable Context Compression: Execution Instability (TRACE)
URL: https://arxiv.org/html/2608.06503v1 (Nokia Applied Research, 2026-08)
- Documents that recurrent compression weakens the influence of recent interactions: more blocked actions, repeated exploration, run-to-run instability.
- TRACE: evaluate each compaction boundary with paired closed-loop continuations from the same environment state (PRE as control, POST measures burden induced by the summary); preference signal optimizes the natural-language compression template; all models frozen.
- On AppWorld: +5.7 accuracy, +7.8 Pass2 (multi-run consistency) over the unoptimized prompt; optimized template transfers across models (MiniMax-M3 -> Kimi-K2.7-Code) and beats Microsoft ACON guidelines (ICML 2026).
- Design implication: compaction quality is measurable per-event with paired rollouts; the compression prompt is a tunable artifact; instability across repeated runs is a first-class metric.

## Learning Agent-Compatible Context Management (AdaCoM)
URL: https://arxiv.org/html/2605.30785 (2026)
- External RL-trained manager edits a frozen agent's context (rewrite/delete/merge messages with justifications, GRPO with process rewards: over-length penalty, redundant-action penalty for repeated identical tool calls, format penalties).
- Fidelity-Reliability trade-off: higher-performing agents benefit from higher-fidelity preservation; weaker agents need more aggressive compression to stay in a reliable regime.
- Process-reward detail worth stealing without the RL: penalize/monitor repeated identical tool calls as a proxy for insufficient preservation.
- Design implication: monitor repeated-identical-tool-call patterns after compaction as a runtime signal of information loss; fidelity defaults should scale with model capability.

## Context Compression for LLM Agents: A Survey of Methods, Failure Modes, and Evaluation
URL: https://doi.org/10.20944/preprints202605.2065.v1 (2026-05)
- Taxonomy: compression target (what), mechanism (how), control policy (who decides).
- Failure taxonomy: F1 pre-compression decision error (wrong time/wrong target), F2 in-compression information loss, F3 post-compression access failure (cannot get it back).
- Design implication: the architecture needs explicit answers to all three: predictive trigger policy (F1), typed operators + validation (F2), lossless store + read-back tool (F3).
