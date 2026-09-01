# CTXEVAL-MIXED-LANES fixture
- constraint: CTXEVAL-CONSTRAINT-K1 "never rewrite the ledger outside a transaction"
- decision: CTXEVAL-DECISION-D2 "keep tool results claim-atomic"
- source snippet: fn fold(&self) -> Fold { Fold::credited(self.range()) }
- failing test identity: CTXEVAL-FAILING-TEST-T7 lane_policy::tests::quoting
- superseded exploration: CTXEVAL-SUPERSEDED-E9 (do not carry forward)
