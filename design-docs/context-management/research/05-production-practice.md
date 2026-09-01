# Production Practice (deployed systems, first-party APIs)

## Anthropic first-party context management APIs
URLs: https://platform.claude.com/docs/en/build-with-claude/compaction , /context-editing , cookbook https://platform.claude.com/cookbook/tool-use/context-engineering-context-engineering-tools (2026)

### Server-side compaction (compact_20260112)
- Trigger: input_tokens threshold, default 150K, minimum 50K. Generates summary in a typed `compaction` block; on subsequent requests everything before the block is dropped by the API.
- Knobs: custom `instructions` (REPLACES the default summarization prompt entirely), `pause_after_compaction` (stop after summary; client can inject preserved blocks before continuing; usable with a compaction counter to enforce total token budgets and wrap up gracefully).
- Multiple compactions per conversation: the last block reflects final state.

### Context editing (clear_tool_uses_20250919, clear_thinking_20251015)
- Sub-transcript operation: walks the message list, replaces old `tool_result` blocks with a placeholder, keeps the `tool_use` record (the model knows the call happened and the body was removed). Server-side, pre-prompt; client keeps full unmodified history.
- Knobs: trigger (default 100K input tokens), keep (default 3 most-recent tool uses), clear_at_least (do not fire unless enough tokens are freed to be worth the cache re-write), exclude_tools (never clear these), clear_tool_inputs (also drop call parameters).
- Thinking-block clearing trades context room against cache: clearing thinking invalidates the prefix cache at that point; keep=all preserves cache.
- Cache rule made explicit: any clearing/compaction invalidates cached prefix tokens; batch enough reclamation to amortize the re-write.

### Memory tool (memory_20250818)
- Model-driven file-backed memory outside the window: view/create/str_replace/insert/delete/rename; client implements storage. Auto-injected protocol prompt tells the agent to assume its context may be reset at any moment, so anything not written to memory is at risk.
- Survives context resets and sessions; costs a tool round-trip per access.

### Cookbook numbers
- Research agent: context editing alone improved agentic-search eval 29%; adding the memory tool: 39%. The delta is the memory tool catching specifics the editing pass drops.
- 100-turn web-search run: clearing cut token consumption 84% while finishing tasks that otherwise died of context exhaustion.
- Clearing dropped 7 of 8 bulky file reads; peak context 335,279 -> 173,137 tokens with no observed fact loss.
- Compaction probe: 3/3 high-level facts retained, 0/3 obscure specifics. Compaction is lossy exactly where un-typed retention loses.

## Division-of-labor synthesis (dreaming.press analysis, 2026-06)
URL: https://dreaming.press/posts/context-editing-vs-compaction-for-long-running-agents.html
- Order strategies by what you can afford to lose: re-fetchable bulk -> clear; gist-sufficient reasoning -> compact; must-not-lose specifics -> memory, written BEFORE compaction can summarize them away.
- Stacking order in practice: clearing first (cheap eviction), compaction at a HIGHER threshold (expensive summarization only when eviction is insufficient), exclude the memory tool's own I/O from clearing, subagents as the architectural fourth move for bulky isolated subtasks.
- KV-cache hit rate is the metric that decides an agent's bill; every context lever pays a cache tax that must be sized.

## Claude Code / deployed agents
- Claude Code in production: compaction for long conversations plus two complementary memory systems for cross-session persistence (Anthropic cookbook).
- Context Compaction Theory (file 01) classifies Codex, Claude Code, Gemini CLI, OpenCode, Goose, Cursor as carry-forward compactors: compacted state is the new base; raw history stays in storage.
- Compaction Cliff (file 02) measured Claude Code's actual /compact prompt: 53% safety-rule survival after one round.
- TRACE (file 02) baselines include OpenClaw and Hermes compaction prompts; ACON (Microsoft) guidelines from ICML 2026 are the strongest published prompt-space baseline.
