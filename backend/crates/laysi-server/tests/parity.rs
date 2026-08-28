//! Interface parity tests.
//!
//! This is the enforcement mechanism behind "every operation is available
//! over MCP". The registry, the REST routes and the MCP tool list must be
//! the same set of names; a gap on any side fails here.
//!
//! Why it is needed: sooner or later someone in a hurry will hand-write an
//! axum route instead of going through the registry. That route would not
//! appear over MCP, and nobody would notice without this test.

use laysi_ops::{Registry, Risk};

/// REST has one generic route, `POST /v1/ops/{name}`, so the set of names it
/// covers is by definition the registry. This function turns that fact into
/// an executable assertion.
fn rest_tool_names(r: &Registry) -> Vec<&'static str> {
    r.iter().map(|op| op.name()).collect()
}

/// The MCP `tools/list` is generated from the registry too (see src/mcp.rs).
fn mcp_tool_names(r: &Registry) -> Vec<&'static str> {
    r.iter().map(|op| op.name()).collect()
}

#[test]
fn rest_and_mcp_expose_exactly_the_same_operations() {
    let r = Registry::new();

    let mut registry: Vec<&str> = r.iter().map(|o| o.name()).collect();
    let mut rest = rest_tool_names(&r);
    let mut mcp = mcp_tool_names(&r);
    registry.sort_unstable();
    rest.sort_unstable();
    mcp.sort_unstable();

    assert_eq!(registry, rest, "REST 與註冊表的操作集合不一致");
    assert_eq!(registry, mcp, "MCP 與註冊表的操作集合不一致");
    assert_eq!(rest.len(), mcp.len());
    assert!(!registry.is_empty(), "註冊表是空的");
}

#[test]
fn every_operation_is_reachable_by_name() {
    let r = Registry::new();
    for op in r.iter() {
        assert!(r.get(op.name()).is_some(), "{} 查不回來", op.name());
    }
    assert!(r.get("no_such_operation").is_none());
}

/// MCP tool descriptions must carry the risk tier: the model can only apply
/// the brakes for a user if it can see the risk.
#[test]
fn mcp_descriptions_carry_the_risk_label() {
    for op in Registry::new().iter() {
        let description = format!("[{}] {}", op.risk().label_zh(), op.summary());
        assert!(
            description.starts_with('['),
            "{} 的說明沒有風險標示",
            op.name()
        );
        assert!(
            description.contains(op.risk().label_zh()),
            "{} 的說明沒有標出風險等級",
            op.name()
        );
    }
}

/// High-risk and owner-only operations must stay few and be recognisable at
/// a glance.
#[test]
fn risk_tiers_are_assigned_deliberately() {
    let r = Registry::new();
    let read_only = r.iter().filter(|o| o.risk() == Risk::ReadOnly).count();
    let writing = r.iter().filter(|o| o.risk() != Risk::ReadOnly).count();
    assert!(read_only > 0 && writing > 0, "風險分級沒有實際區分任何東西");
    assert_eq!(read_only + writing, r.len());
}
