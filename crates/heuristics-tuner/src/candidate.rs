//! A candidate system prompt and how it was produced.

/// Where a candidate prompt came from — carried through to the final rubric so it can explain the
/// search path (a cold-start that wins is more surprising, and worth flagging, than a
/// mutation-of-the-current-best that wins).
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateOrigin {
    /// Prompted from scratch, independent of the current beam — the Monte Carlo restart that
    /// keeps the search from getting stuck near wherever the first candidate happened to land.
    ColdStart,
    /// Mutated from another candidate's text.
    MutatedFrom {
        /// Position of the parent in the beam it was mutated from.
        parent_index: usize,
        /// The parent's own accuracy, carried along so the rubric can show lineage without a
        /// second lookup.
        parent_accuracy: f32,
    },
}

/// A candidate system prompt. Pure data — fitness is computed and carried separately (see
/// `scoring::CandidateFitness`) so this stays trivially constructible in tests.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub prompt: String,
    pub origin: CandidateOrigin,
}
