//! `records-stream/v1`, the NDJSON stream a `health-records/v1` provider
//! writes for `records export --ndjson`: a header, one FHIR resource per
//! line, and a trailer. The types and the reader are medical-common's
//! `pk-med-records`; this module names what medrec uses of it.

pub use pk_med_records::{
    Header, Item, RawResource, RecordsReader, Skip, StreamError, Trailer, END_SCHEMA,
    MAX_LINE_BYTES, PROFILE, STREAM_SCHEMA,
};
