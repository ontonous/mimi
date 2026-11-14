// The collector supplies the exact generated bundle source only after it has
// validated the generation manifest, expectations, source hashes, and target.
// Direct Cargo use with this feature and no validated bundle fails at compile
// time because the environment variable is intentionally absent.
include!(env!("MIMI_ROUTE_CENSUS_AUTHORITY_SOURCE"));
