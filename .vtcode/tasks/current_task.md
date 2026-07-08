# Add title attribute to Send button in chat.rs

- [ ] Locate the Send button `<button>` in `crates/webui/src/components/chat.rs` and add `title="Send (Enter)"`.
  outcome: The `<button>` element now has `title="Send (Enter)"`.
  verify: grep -n 'title="Send (Enter)"' crates/webui/src/components/chat.rs
- [ ] Ensure no other files were modified.
  outcome: Only the target file is changed.
  verify: git diff --stat --name-only  # Should only show crates/webui/src/components/chat.rs
- [ ] Confirm the Send button text remains "Send" (or whatever the original text was).
  outcome: The button text is unchanged.
  verify: grep -A2 'title="Send (Enter)"' crates/webui/src/components/chat.rs | grep -o 'Send[^<]*'
