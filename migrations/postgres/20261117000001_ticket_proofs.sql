-- Management-ticket proofs already accepted, so a proof is used once. A row
-- lives until its ticket has expired (forget_at, Unix seconds), after which
-- the ticket itself is refused; expired rows are swept as new ones arrive.
CREATE TABLE happyview_ticket_proofs (
    proof_key TEXT PRIMARY KEY,
    forget_at BIGINT NOT NULL
);
CREATE INDEX happyview_ticket_proofs_forget_at ON happyview_ticket_proofs (forget_at);
