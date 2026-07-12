---
name: consensus
description: Third-model arbitration of ONE genuinely contested finding or design question via pal-mcp-server's stance-steered consensus (needs the pal MCP server + a Gemini key). Use ONLY when cross-model review left a critical/major disagreement unresolved — the evidence says ungrounded debate is the weakest pattern, so this is a last resort, not a routine step.
argument-hint: "[the single contested question or finding, stated neutrally]"
---

# Consensus — third-model arbitration

Two models disagreed and neither rebuttal landed. Before a human has to
arbitrate blind, run ONE stance-steered debate through a third vendor and
synthesize the result. This exists for genuinely contested calls — if you can
settle the question with a test, run the test instead.

## Procedure

1. **Check availability.** The `mcp__pal__consensus` tool must be present
   (pal-mcp-server installed and a Gemini key configured). If it is not,
   say so in one line and stop — do not simulate a third opinion yourself.

2. **State the question neutrally.** One contested item per invocation.
   Strip any hint of which model held which position; include the concrete
   context both sides agreed on (the code, the failure scenario, the
   constraint) in the prompt.

3. **Run one round.** Call `mcp__pal__consensus` with two steered stances —
   one arguing FOR the proposition, one AGAINST — and the neutral question.
   ONE round only: no follow-ups, no re-rolls until it agrees with you.

4. **Synthesize a verdict.** One paragraph: what the arbiter concluded, the
   single strongest argument on each side, and a concrete recommendation
   (accept / reject / needs-a-test). Label it clearly as a third-model
   opinion, not ground truth.

## Guardrails
- Never use this to GENERATE findings or designs — it arbitrates one
  existing disagreement, nothing else.
- One round, then a human. If the verdict still feels contestable, that is
  the signal to write a discriminating test, not to keep debating.
- If the disagreement dissolves on restating it neutrally (the models were
  answering different questions), report that instead of running the tool.
- Cost awareness: this burns three models on one question. Reserve it for
  decisions where being wrong is expensive.
