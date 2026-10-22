---
name: writing-commit-messages
description: >-
  Writes Git commit messages. Activates when the user asks to write
  a commit message, draft a commit message, or similar.
---

# Writing Commit Messages

Write commit messages that follow commit style guidelines for the project.

## Format

```
fix(tmuxx): retain pane CWDs across reconnects

- Restore directories for panes without shell CWD reports.
- Keep older peers compatible through an optional metadata field.
- Validate reconnects and lightweight updates.
```

## Rules

### Subject line

- **Conventional Commits**: Use `<type>(<scope>): <summary>`, such as
  `fix(tmuxx): retain pane CWDs across reconnects`. Choose a type matching the
  change (`fix`, `feat`, `docs`, `refactor`, `test`, `build`, `ci`, or `chore`)
  and a concise scope when useful.
- **Compatibility**: Mark incompatible changes with `!` after the type/scope
  and a `BREAKING CHANGE:` footer describing the impact and migration.
  Backward-compatible additions need neither marker.
- **Summary**: Lowercase start (not capitalized), imperative mood,
  no trailing period. Keep it concise—ideally under 60 characters
  total for the whole subject line.

### References

- If the change relates to a GitHub issue, PR, or discussion, list
  the relevant numbers on their own lines after the subject, separated
  by a blank line. E.g. `#1234`
- If there are no references, omit this section entirely (no blank
  line).

### Markdown changelog body

- Prefer concise bullets describing behavior changes, fixes, compatibility,
  and relevant validation.
- Explain user impact and motivation where useful; avoid detailed accounts
  of implementation mechanics or the development process.
- Wrap prose at roughly 72 characters. Omit a body when the subject is enough.

## Workflow

- If `.jj` is present, use `jj` instead of `git` for all commands.
- Run a diff to see what changes are present since the last commit.
- Identify the subsystem from the changed file paths.
- Identify any referenced issues/PRs from the diff context or
  branch name.
- Draft the commit message following the format above.
- Apply the commit
- Don't push the commit; leave that to the user.
