/// Locked confidence mapping from the implementation plan.
pub const CONF_EXPLICIT: f32 = 1.00;
pub const CONF_DIRECT_IN_ROOT: f32 = 0.95;
pub const CONF_DIRECT_OUTSIDE_ROOT: f32 = 0.90;
pub const CONF_REPEATED: f32 = 0.85;
pub const CONF_PLAUSIBLE: f32 = 0.75;
pub const CONF_DEPENDENCY: f32 = 0.30;
pub const CONF_AMBIENT: f32 = 0.10;

pub const OWNERSHIP_STRONG: f32 = 0.85;
pub const OWNERSHIP_CANDIDATE_MIN: f32 = 0.75;
pub const OWNERSHIP_CANDIDATE_MAX: f32 = 0.849;
pub const DEPENDENCY_MIN: f32 = 0.30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationStrength {
    StrongOwnership,
    Candidate,
    Dependency,
    Ignore,
}

pub fn association_strength(confidence: f32) -> AssociationStrength {
    if confidence >= OWNERSHIP_STRONG {
        AssociationStrength::StrongOwnership
    } else if (OWNERSHIP_CANDIDATE_MIN..OWNERSHIP_STRONG).contains(&confidence) {
        AssociationStrength::Candidate
    } else if confidence >= DEPENDENCY_MIN {
        AssociationStrength::Dependency
    } else {
        AssociationStrength::Ignore
    }
}

pub fn clamp_confidence(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

/// Reads prove use, never ownership on their own.
pub fn ownership_from_read_only() -> f32 {
    CONF_DEPENDENCY
}
