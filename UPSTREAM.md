# Agent2API SDK / Protocol

`wasm/sdk` is an unmodified snapshot of the WASM SDK from
[Agent2Providers at `b35b163d`](https://github.com/ProvidersPlugin/Agent2Providers/tree/b35b163d01fc3effb30ae40a012fd657fdc0a54d/wasm/sdk).
The compatibility target is [Agent2API v3.5.1](https://github.com/aimod-cc/Agent2API/tree/v3.5.1).

## Legacy native implementation

`crates/provider-sdk` and `crates/provider-protocol` are unmodified snapshots of
[Agent2Providers at `3055f416841c`](https://github.com/ProvidersPlugin/Agent2Providers/tree/3055f416841c1393ec04294a702603fd41e840aa)
under the included AGPL-3.0 license. They are vendored so this repository builds
independently of the upstream default branch. This repository contains only the
`wps-lingxi` provider, not the upstream provider bundle or the Agent2API gateway.
