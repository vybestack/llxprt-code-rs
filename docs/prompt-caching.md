# Prompt caching on stateless HTTP

The system prompt and ordered tool definitions are an immutable session head. Do not
put timestamps, run IDs, remaining budgets, or per-round state there. Conversation
content is appended after that head. Changing cwd, shell enablement, configured tool
budget, reasoning notes, or tool definitions deliberately invalidates the head. The
compiled `prompt_cache` test compares actual serialized HTTP substrings across two
rounds and two process-per-turn invocations, with mutation and tool-order controls.
There is no epoch-summary prompt API wired into this pre-kernel runtime.

## Settings

`ephemeralSettings.prompt-caching` defaults to enabled. `off` disables explicit
breakpoints/routing; it does not prohibit a provider's implicit caching. Anthropic
accepts only `off` or absence, marking the last tool and system block with ephemeral
cache control. Its default TTL is five minutes. No changing transcript is marked.
OpenAI Chat and Codex accept `off`, `1h`, `24h`, or absence; these enable/disable
session routing, not a TTL promise. Public OpenAI Responses retains its existing
24-hour retention request when enabled. All OpenAI paths use the session label as
`prompt_cache_key`, not as response continuation. Chat's session-owned routing field
takes precedence over a same-named arbitrary model parameter, including when off.
Never use secrets in session labels. Concurrent independent sessions should use
different labels. The default label is `default`, not a generated unique identity.

## Observations

Every successful provider completion emits two JSON lines to stderr, separate from
the exactly-one-object stdout envelope:

* `prompt_cache_call`: call ordinal, raw reported input, cached-read input, cache
  creation input, uncached input, total input, and denominator convention.
* `prompt_cache_run`: cumulative completed calls, measured calls, measured total and
  cached input, and token-weighted `hit_ratio` for this process invocation. The last
  line is the run aggregate, including completed calls before a later failure.

OpenAI reports input inclusive of cache reads. Uncached input is input minus reads.
Anthropic reports input excluding both cache reads and creation. Total input is the
sum of all three; uncached input includes creation. Creation is separately exposed
because it has a different price. No currency savings are inferred.

Missing provider counters remain JSON null, not zero, including absent Anthropic
input usage. Inconsistent external counters (reads exceeding inclusive input, or
an overflowing per-call sum) preserve the raw values but omit invalid derived
values and do not enter the ratio. A call enters the ratio only
when its total denominator and cached-read count are known. A zero-token denominator
produces null ratio. Cache-off calls can still report implicit hits. Failed transports
have no completion usage and do not enter the aggregate. These numbers describe
provider-reported tokens, not a local tokenizer estimate or a request-flag prediction.
Matched enabled/disabled measurements should report missing usage or no benefit as
observations, not claim latency or cost savings without evidence.
