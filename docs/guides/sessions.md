# Sessions: resume, pause, undo, export, delete

Every conversation, in the terminal UI or with `harness run`, is a session saved in the
project's `.harness/state.db`. Nothing is lost: each event is saved before harness acts
on it, so even a crash or `kill -9` leaves a session you can continue.

## Continue a conversation

In the terminal UI:

- **`harness -c`** (`--continue`) opens this project's most recently active session.
- **`/sessions`** lists the project's sessions, most recently active first. Pick one with
  ↑↓ and Enter, or its number, to switch to it. The one you leave stays saved.
- Starting plain `harness` shows the last session on the first line, with how to get back.
- The header ends with the session's short id; `harness --session ID` opens any session.

Headless:

```sh
harness run "add input validation to the signup form"
# … later
harness session list
harness run --session 01M7P3 "now write tests for it"
```

Ids can be shortened to any unique prefix. A session keeps the model it started with.
A session nobody typed into is deleted when the terminal UI leaves it, so opening and
closing harness leaves nothing behind.

## Pause and resume

- **Ctrl-C** interrupts at once: the model stops, a running command is killed, and
  the task is paused. Ctrl-C twice exits.
- **`harness task pause ID`** (from another terminal) pauses at the next safe point:
  before the next model call, after the current tool, or after an approval. A running
  command is never cut off by a pause.
- **`harness session resume ID`** continues the paused task, in a new process if you like.
  Calls that already finished are not run again.

```sh
harness task list
harness task pause 01M7P3XQ
harness session resume 01M7P3XQA8
harness task cancel 01M7P3XQ        # or give up on it
```

## After a crash

Reopening a session whose task was running when harness stopped recovers it:

- A tool call that was running gets the result "The harness stopped while this was
  running. Outcome unknown." It is never re-run automatically.
- Calls that had not started go through the permission rules again when you resume
  (they may ask again).
- An unfinished model reply is dropped; resuming asks the model again.
- The task is left **paused**; `harness session resume ID` continues it.

## Undo

```sh
harness session undo ID
```

Restores every file that `write_file`, `edit_file` or `apply_patch` changed in the
session's last turn (files the turn created are deleted). Run it again to go one turn
further back. The model is told which files were reverted. Shell commands are not
covered: use git for those.

## Look and export

```sh
harness session show ID                       # every event, one per line
harness session export ID                     # Markdown transcript in .harness/exports/
harness session export ID --format jsonl --output session.jsonl
```

## Delete

```sh
harness session delete ID          # asks first
harness session delete ID --yes    # in scripts
```

Removes the session and everything stored for it. This cannot be undone. Stored files
that no other session uses are freed too (when no other session is open). A session
that is open in a harness process cannot be deleted.

## Files

`.harness/` holds `state.db`, `blobs/`, `locks/` and `exports/`, and a `.gitignore`
that keeps them out of git. Only one harness process can run a session at a time; the
others can still list, show and export it, and send pause, resume or cancel requests.
