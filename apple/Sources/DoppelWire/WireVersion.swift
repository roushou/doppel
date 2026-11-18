// Each channel's wire protocol version, as `Hello.protocolVersion` carries
// it. Rust defines the same values (`Protocol::VERSION` in doppel-protocol's
// exchange.rs, with when each may change); the golden wire fixtures hold
// both sides to them.

public enum WireVersion {
    /// Host and agent. Golden images keep the agent they were prepared with,
    /// so this grows by fields and capabilities, not by bumping.
    public static let agent: UInt32 = 1
    /// Host and VMM, always from one build.
    public static let vmm: UInt32 = 1
}
