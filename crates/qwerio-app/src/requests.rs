//! Tickets that let views ignore results from superseded requests.

/// Identifies one request for a view. A result is applied only if its ticket
/// is still the latest one issued.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket {
    generation: u64,
    sequence: u64,
}

/// Issues tickets. The generation changes whenever the query's identity
/// changes (filters, sort, metadata, connection), which also invalidates
/// continuation; the sequence changes for every request.
#[derive(Clone, Debug, Default)]
pub struct TicketIssuer {
    generation: u64,
    latest: Option<Ticket>,
    sequence: u64,
}

impl TicketIssuer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn issue(&mut self) -> Ticket {
        self.sequence += 1;
        let ticket = Ticket {
            generation: self.generation,
            sequence: self.sequence,
        };
        self.latest = Some(ticket);
        ticket
    }

    /// Supersedes every outstanding ticket.
    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.latest = None;
    }

    pub fn is_current(&self, ticket: Ticket) -> bool {
        self.latest == Some(ticket)
    }

    /// Marks the latest ticket as answered so duplicates are ignored.
    pub fn complete(&mut self, ticket: Ticket) -> bool {
        if self.is_current(ticket) {
            self.latest = None;
            return true;
        }

        false
    }

    pub fn is_pending(&self) -> bool {
        self.latest.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_ticket_is_accepted_once() {
        let mut issuer = TicketIssuer::new();
        let first = issuer.issue();
        let second = issuer.issue();

        assert!(!issuer.complete(first));
        assert!(issuer.complete(second));
        assert!(!issuer.complete(second));
    }

    #[test]
    fn invalidation_rejects_outstanding_tickets() {
        let mut issuer = TicketIssuer::new();
        let ticket = issuer.issue();

        issuer.invalidate();

        assert!(!issuer.complete(ticket));
        assert!(!issuer.is_pending());
    }
}
