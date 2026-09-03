//! Connector OAuth flows against in-process fakes, merged into one
//! integration binary so the crate graph links once instead of per file.

mod chatgpt_flow;
mod gateway_flow;
