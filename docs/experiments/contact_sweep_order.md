# Alternating contact sweeps: rejected experiment

Baseline commit: `0dac0f8882351043b832f0dcad032155f8f49127`.

This experiment was implemented and measured, then rejected. Production solver configuration and loops were restored exactly. The companion [patch](../../tools/experiments/alternating_contact_sweep.patch) contains the complete candidate, including its public Rust configuration field and integration acceptance test. The field is absent from the shipped library.

## Registered plan

Keep speculative margin 0.005, 20 velocity sweeps, eight position sweeps, and one existing relaxation sweep. Alternate forward and reverse **contact point** traversal on successive velocity sweeps, starting forward. Reversal includes points within a manifold. Joint solves remain after each contact sweep. Serial, parallel island batching without joints, and parallel iteration with joints all use the same parity. Warm starting, position correction, relaxation, material settings, fixture geometry, and iteration budget remain unchanged.

Acceptance was registered before candidate measurement: all of 4, 12, and 32 layers must meet 5mm plus 10µm tolerance for both independent all-pair OBB SAT overlap and finite floor overlap across 1800 actual public steps at 60Hz (30 seconds). Only after passing precision would a production-library example measure 12/32 layers × Forward/Alternating, eight warmup plus 120 measured steps, five trials per case, alternating trial order. Each layer's median regression must be at most 15%. The timing stage was **not run**, because precision failed. No performance improvement is claimed.

## Actual behavior

All worlds use side-0.5 boxes, mass 1, initial centers `y=0.25+i*0.502`, and finite floor half extents `(4,0.25,4)` centered at `(0,-0.25,0)`. Every poststep checks independent f64 OBB SAT for all body pairs and the finite floor, finite public states, body/mass counts, actual steps, and contact presence. All three worlds complete before the acceptance assertion.

| Layers | Forward pair peak (mm) | Alternating pair peak (mm) | Forward floor peak (mm) | Alternating floor peak (mm) |
|---:|---:|---:|---:|---:|
| 4 | 1.299087 | 1.375492 | 1.097863 | 1.294199 |
| 12 | 3.537737 | 3.709160 | 3.593264 | 3.896402 |
| 32 | 436.883123 | 469.318804 | 201.288257 | 133.577233 |

The explicit Alternating stub initially executed unchanged Forward loops, successfully compiled, and produced a real assertion failure (exit 101). The implemented candidate also successfully compiled and failed (exit 101), with the same fixed acceptance criterion. Each run completed three worlds / 5400 actual steps. The latter 32-layer pair peak occurred at step 97 and floor peak at step 205. All 32 bodies slept by step 364, with maximum horizontal center drift 4.073655m and maximum absolute footprint coordinates X=4.118599m, Z=4.146712m. There were zero wholly-outside body-step samples, which does not exclude partial overhang. Sleep did not establish precision or upright stability: the stack had collapsed. The worse pair overlap violates the registered target; reduced floor overlap is insufficient to accept the candidate.

No threshold, fixture, iteration budget, or fallback acceptance rule was changed after measurement. No production candidate or unused public field remains. No timing example was created. Snapshot behavior was not changed: existing snapshots do not capture solver configuration; caller configuration remains runtime state.

## Reproduce the candidate

Use an isolated checkout at the exact baseline above. The patch changes only the solver, world, parallel solver, and stack integration test. Its SHA-256 is `72ab8996a5618cdcfa5445d6814dc87bfd8a4541c934e9d0b3116908fcef10b4`.

```bash
git apply --check tools/experiments/alternating_contact_sweep.patch
git apply tools/experiments/alternating_contact_sweep.patch
cargo test --release --offline --locked -p quench3d-physics \
  --test stack_precision alternating_sweep_sustained_precision_target -- --nocapture
```

The final command is expected to fail on the 32-layer target. Use Rust 1.99.0 and the existing offline workspace dependencies. It must compile and execute; a compilation failure is not the recorded result. To replay the stub, preserve the patch's configuration/test additions but restore all solver execution loops to the baseline before running the same command; that was the measured preimplementation red. The archived patch is the complete implemented candidate, not the stub.

Restored baseline source SHA-256 values:

| Source | SHA-256 |
|---|---|
| [solver](../../crates/quench3d-physics/src/solver.rs) | `f20fd98bba7b0f99446b32f9907c004572d9b5205e8fd82e4cb787c0e56d4192` |
| [world](../../crates/quench3d-physics/src/world.rs) | `d98a1c849b715c1a027afc4874d47b60af5ce0492f16aa17f04fa573ba6f0bb9` |
| [parallel](../../crates/quench3d-physics/src/parallel.rs) | `7bdd1ccb43d363adc0a19d909101bd0f2ce08b696220d5ed81ec77e24411b5b3` |
| [stack integration](../../crates/quench3d-physics/tests/stack_precision.rs) | `c8ccd6a0f4712199a878671d1a5509c1515b1a768265c3a922e6ed82ef5aba72` |

The patch applies cleanly to the restored baseline (`git apply --check` passed), and all four production/test source files match baseline exactly. Local original logs and exit statuses remain under `target/tmp/round177-contact-sweep/`; those ignored runtime artifacts are supplementary evidence, while this record and patch survive cleaning that directory.
