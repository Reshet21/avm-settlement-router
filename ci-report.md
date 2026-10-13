## AVM settlement router verification

- commit: `4267a80684afb0b48bf8ddaff70d854c9074355e`
- toolchain: rustc 1.93.0 (254b59607 2026-01-19) / rustfmt 1.8.0-stable (254b59607d 2026-01-19)
- state image: state image     = 448 bytes

### Test results
```
## sdk canonical tests
running 8 tests
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.35s
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
## router integration tests (canonical ACMI frames)
[1m[33mwarning[0m: profiles for the non root package will be ignored, specify profiles at the workspace root:
package:   /home/runner/work/avm-settlement-router/avm-settlement-router/beneficiary/Cargo.toml
workspace: /home/runner/work/avm-settlement-router/avm-settlement-router/Cargo.toml
[1m[92m   Compiling[0m avm-settlement-router v0.1.0 (/home/runner/work/avm-settlement-router/avm-settlement-router)
[1m[92m    Finished[0m `test` profile [unoptimized + debuginfo] target(s) in 0.32s
[1m[92m     Running[0m tests/settlement_router.rs (target/debug/deps/settlement_router-6b906d73cd02cc64)

running 29 tests
test a_non_full_replace_descriptor_requirement_is_refused ... ok
test an_unknown_method_and_a_corrupt_state_fail_closed ... ok
test callback_needs_an_internal_caller_and_a_settlement_in_flight ... ok
test callback_records_a_bounced_settlement_and_blocks_a_replay ... ok
test code_upgrade_rejects_the_current_code_id_and_zero_hashes ... ok
test callback_records_a_completed_settlement_and_rejects_a_replay ... ok
test callback_with_a_body_and_returned_value_completes_the_settlement ... ok
test failure_flow_sentinel_selects_the_malformed_output_channel ... ok
test code_upgrade_requires_the_owner_and_emits_an_intent ... ok
test failure_flow_settle_sentinel_selects_the_receiver_trap ... ok
test failures_carry_no_effects ... ok
test init_installs_the_declared_state_image ... ok
test init_installs_the_deployers_key_for_the_acdp_package_envelope ... ok
test init_installs_the_deployers_key_when_the_deploy_body_is_empty ... ok
test init_reads_the_owner_key_from_the_acdp_init_payload ... ok
test init_installs_an_owner_key_from_the_deploy_body ... ok
test route_administration_rejects_a_stranger_and_a_self_destination ... ok
test owner_can_install_update_and_clear_routes ... ok
test route_table_is_bounded_by_max_routes ... ok
test settle_forwards_the_full_value_and_requests_a_callback ... ok
test only_one_settlement_may_be_in_flight ... ok
test settle_rejects_a_malformed_body_and_a_self_beneficiary ... ok
test settle_rejects_an_internal_caller_and_transferred_extra_currency ... ok
test settle_rejects_an_unknown_route_and_a_beneficiary_mismatch ... ok
test settle_rejects_value_below_the_route_minimum ... ok
test version_probe_answers_only_an_external_empty_call ... ok
test withdraw_drains_value_and_extra_currency_to_the_owner ... ok
test the_beneficiary_fixture_answers_the_routers_delivery ... ok
test withdraw_rejects_a_stranger_a_body_and_an_empty_balance ... ok

test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

## router integration tests (AVM_E2E_FAILURE_FLOW=1 fixture build)
[1m[33mwarning[0m: profiles for the non root package will be ignored, specify profiles at the workspace root:
package:   /home/runner/work/avm-settlement-router/avm-settlement-router/beneficiary/Cargo.toml
workspace: /home/runner/work/avm-settlement-router/avm-settlement-router/Cargo.toml
[1m[92m   Compiling[0m avm-settlement-router v0.1.0 (/home/runner/work/avm-settlement-router/avm-settlement-router)
[1m[92m    Finished[0m `test` profile [unoptimized + debuginfo] target(s) in 0.32s
[1m[92m     Running[0m tests/settlement_router.rs (target/debug/deps/settlement_router-6b906d73cd02cc64)

running 29 tests
test a_non_full_replace_descriptor_requirement_is_refused ... ok
test an_unknown_method_and_a_corrupt_state_fail_closed ... ok
test callback_needs_an_internal_caller_and_a_settlement_in_flight ... ok
test callback_records_a_bounced_settlement_and_blocks_a_replay ... ok
test callback_records_a_completed_settlement_and_rejects_a_replay ... ok
test code_upgrade_rejects_the_current_code_id_and_zero_hashes ... ok
test code_upgrade_requires_the_owner_and_emits_an_intent ... ok
test callback_with_a_body_and_returned_value_completes_the_settlement ... ok
test failure_flow_sentinel_selects_the_malformed_output_channel ... ok
test failure_flow_settle_sentinel_selects_the_receiver_trap ... ok
test failures_carry_no_effects ... ok
test init_installs_an_owner_key_from_the_deploy_body ... ok
test init_installs_the_declared_state_image ... ok
test init_installs_the_deployers_key_for_the_acdp_package_envelope ... ok
test init_installs_the_deployers_key_when_the_deploy_body_is_empty ... ok
test init_reads_the_owner_key_from_the_acdp_init_payload ... ok
test only_one_settlement_may_be_in_flight ... ok
test owner_can_install_update_and_clear_routes ... ok
test route_table_is_bounded_by_max_routes ... ok
test settle_forwards_the_full_value_and_requests_a_callback ... ok
test settle_rejects_a_malformed_body_and_a_self_beneficiary ... ok
test route_administration_rejects_a_stranger_and_a_self_destination ... ok
test settle_rejects_an_internal_caller_and_transferred_extra_currency ... ok
test settle_rejects_an_unknown_route_and_a_beneficiary_mismatch ... ok
test settle_rejects_value_below_the_route_minimum ... ok
test version_probe_answers_only_an_external_empty_call ... ok
test the_beneficiary_fixture_answers_the_routers_delivery ... ok
test withdraw_drains_value_and_extra_currency_to_the_owner ... ok
test withdraw_rejects_a_stranger_a_body_and_an_empty_balance ... ok

test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

## in-crate codec tests
running 11 tests
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### Artifacts
```
-rwxr-xr-x 2 runner runner 52130 Oct 13 05:13 target/wasm32-unknown-unknown/release/avm_settlement_router.wasm
04b02028fc24b2979d7f0233c118b68fa9d25bacf1098dd94f748d829d308c91  target/wasm32-unknown-unknown/release/avm_settlement_router.wasm
```

<!-- ci-verification -->
