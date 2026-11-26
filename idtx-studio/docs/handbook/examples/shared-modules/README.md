# Shared Rust module extension

Copy the behaviors/ and extensions/ files into a writable Signal Room project.
Keep their relative paths and preserve the existing project.toml and input map.
Add @../extensions/shared-modules.usda@ to the subLayers array in levels/hub.usda,
then save and reopen Hub in Conductor. This extension adds SharedYaw and
SharedRoll: two separate behaviors that import one shared/motion.rs helper.

Always begin behavior editing in Conductor's Inspector > Behavior panel. Open
extensions/shared-modules.usda in Conductor so its bindings are locally editable.
Select SharedYaw and choose Edit Code, then do the same for SharedRoll. Both
entries are registered in the generated .idtx/behavior-ide/behaviors.code-workspace.
Use Set up rust-analyzer if prompted and trust your project's workspace.

In VS Code, go to the definition of motion::advance_angle from either entry:
both lead to the same behaviors/shared/motion.rs. The helper has no behavior
attachment, UUID or generated crate of its own. Change SPEED_SCALE from 1.0 to
2.0 there, save all files, and run Cargo Check and Verify capabilities for both
bindings. Return to Hub and start a fresh Desktop preview: both objects should
rotate twice as fast, around different axes. Restore SPEED_SCALE afterwards.

The complete walkthrough and all four authored listings are in the accompanying
handbook's src/tutorial/07-shared-modules.md. This is an optional Signal Room
extension; it does not have its own project.toml or require a Cargo.toml.
