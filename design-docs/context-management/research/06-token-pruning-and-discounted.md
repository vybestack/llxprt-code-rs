# Token Pruning and Other Discounted Approaches

## LLMLingua family (LLMLingua, LongLLMLingua, LLMLingua-2)
URLs: https://aclanthology.org/2024.acl-long.91/ (LongLLMLingua), survey https://aclanthology.org/2025.naacl-long.368.pdf
- Perplexity/classifier-based token deletion using a small LM; ratios to 20x; LongLLMLingua adds question-aware coarse-to-fine selection, document reordering against position bias, dynamic ratios; +21.4% NaturalQuestions at ~4x fewer tokens (GPT-3.5).
- Discounted for agent trajectories: token pruning optimizes information density, not temporal/structural fidelity; exact strings (paths, error text) are exactly what perplexity-based filters drop or mangle.

## Prompt Compression in the Wild (latency/memory study)
URL: https://arxiv.org/html/2604.02985 (2026)
- 30,000+ queries, 5 open models, 3 GPU classes: only LLMLingua-2 is practical; gains are prefill-phase only; speedups >1.3x require long prompts (>10K) plus high ratios (>4x); code completion is among the most quality-sensitive tasks under pruning.
- Discounted: our workload is coding-dominant; the study's own data rules the family out for code.

## Prompt Compression for LLMs: A Survey (NAACL 2025)
URL: https://aclanthology.org/2025.naacl-long.368.pdf
- Catalogs information loss, fine-tuning brittleness (soft-prompt encoders must retrain per decoder), and limited real efficiency gains.
- Useful contrast: hard-prompt filtering can degrade grammaticality and shift input distribution.

## MemoryCPT (2608.04843) and Memory-Augmented Compression (2608.21265)
- MemoryCPT: end-to-end trainable memory pipeline (query-agnostic distillation + query-aware retrieval/summarization with GRPO, Quality-per-Cost metric). Memory-Augmented CoT: reusable reasoning memories compensate compressed reasoning traces (+21-29 pp over Chain-of-Draft).
- Discounted as primary architecture: both require trained compactors/summarizers we cannot host for closed API models; the Quality-per-Cost metric and the substitution principle are worth borrowing for evaluation framing.

## RL-trained folding/management (FoldGRPO, SUPO, ACON-trained)
- FoldGRPO's process rewards are the informative part (see file 03); training requires model weights and trajectories. Discounted as a dependency; adopted as policy targets the harness can approximate deterministically (branch budget enforcement, scope checks).

## External RL context manager (AdaCoM)
- See file 02. Discounted as primary: requires training an external manager; the fidelity-reliability trade-off and repeated-tool-call signal are adopted as diagnostics.

## Full-history re-derivation per turn
- Discounted by cost (Context Compaction Theory worked example: $13 and ~26 min per re-derivation at 800K history; output cost uncached) and by deployed practice (all surveyed agents carry compacted state forward).

## Uniform summarization-only (/compact-style)
- Discounted by the Compaction Cliff (53%/10% safety-rule survival) and by Context-Folding's result that summarization-based management underperforms folding at equal budget.

## Multi-agent fan-out as primary context strategy
- Discounted as the core mechanism (handcrafted, problem-specific workflows, resists optimization; Context-Folding related work), but retained as an optional isolation mechanism for bulky subtasks (production practice file 05: subagents as fourth move).

## KV-cache/infra-level management (SideQuest, StreamingLLM, H2O, Infini-attention)
- Operate below the API boundary; not available to a harness over closed providers. Discounted; informs the cache-economics constraints the harness must respect instead.
