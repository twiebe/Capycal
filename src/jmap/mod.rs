//! JMAP Calendars (RFC 8620, JSCalendar events): a source and a sink. The
//! pipeline stays on iCal, `convert` translates at the edge.

pub mod client;
pub mod convert;
pub mod source;
