# Agent Notes for cranvr

- You are the repository's sole owner: nobody else finishes what you leave. Take every change from its root cause to merged, measured and cleaned up. Run the checks a change touches before you push it. Close open threads, such as an unpushed branch or a stale issue checkbox, without waiting to be asked.
- Handle every part a change touches: the Mac app (`app/`), the host (`host/`), the protocol (`proto/`), the headset client (`client/`, also its phone build) and the game support (`third_party/`, `steamvr/`). A change is not done while one side lacks it.
- Never write absurd, wasteful code, and remove any you spot in the codebase as soon as you spot it.
- Tests: write only e2e and integration tests through public entry points, plus tests for corner cases that can break behavior. No unit tests of implementation details (private state or storage, internal structure, call order, type sizes, allocation capacities, pointer reuse): they test nothing and duplicate the code. Test bodies go under `/test*/`, never beside implementation: declare them with `#[cfg(test)] #[path = "tests/<name>.rs"] mod tests;`. Measure performance with the e2e harness and the host's own counters.
- Check branch/status at start and completion and after relevant git operations; isolate concurrent work in a worktree of its own. Never use `git reset`; preserve work with a stash when needed. Preserve unrelated and uncommitted work.
- When done with a worktree, push its work and remove it with `git worktree remove`. Never `rm` a worktree, `target/` or `build/` by hand.
- Keep tool results near 2,000 tokens by default; return relevant excerpts, failures and changed results. Save full logs to files and expand only when needed.
- Batch independent reads/checks. Wait 30–60 seconds for long jobs when supported; report progress between waits. Avoid tight polling, repeated status checks and unchanged log dumps.
- Run targeted checks after meaningful edits and required broad checks before integration; rerun only for changed code, failures or new risks. Documentation-only edits need link/format validation, not product builds.
- Work without subagents unless requested. After two passes with no measurable progress, revisit the hypothesis or reference and change approach. Do not declare unfinished work complete.

## Project rules

- cranvr is unreleased: it supports only the current protocol, apps, settings file and game support. A change that bumps a version or a format replaces the old form. The Mac and the headset speak one protocol version. A mismatch shows a plain message that names the side to update.
- The headset client uses only Cranpose's public API and plain Rust crates. When the cause of a client bug or cost lies in Cranpose, fix it in Cranpose, then move the client's pin to the new revision. Never work around a Cranpose bug in cranvr.
- No game hardcoding: per-game choices are visible settings in the app, never code lists of game names.
- Never bypass, patch, disable or hide from anti-cheat.
- Hidden panels and displays cost nothing. Measure it.
- The repository is public. Never write personal data into code, commits, PRs, issues or comments: home paths, device serials, home network addresses, host names, account names. Write `SERIAL` for a device serial.
- Every manual step a user needs (a setting, a dialog, a file copy, a launch option) is a task for the app to automate.

## The headset in tests

- Pass the serial with `-s` to every `adb` command when more than one device is attached.
- Take the lock `/tmp/quest-device.lock` with `mkdir` for every headset step and release it with `rmdir`, also after a failure.
- Wake the headset only with `adb shell am broadcast -a com.oculus.vrpowermanager.prox_close`.
- Never set a system property and never change a system setting of the headset. Never uninstall or clear `dev.cranvr.client`: it holds the user's pairing and settings. Install test builds with `adb install -r`.
- The installed app's host listens on port 9950. Test hosts use ports 9961 and 9962.
- Screen captures use the virtual room (`--es room scene`). Never commit a capture that shows a real room.
- Keep every test, measurement and device run as short as the proof needs. If 1 s gives a clear answer, stop there. Run longer only when the claim is about time itself, such as drift, heat or a rare stall.

## Code rules

- KISS, DRY and SOLID; ten or more duplicated lines need a shared abstraction. Fix root causes and audit every consumer of a wrong value. Leave no partial fixes, deprecated paths or compatibility layers. Review architecture, correctness and maintainability before completion.
- One concept per module, named after its main item; a parent module holds only `mod` and `pub use`. Name the owning module before adding code.
- Types carry the rules: newtypes for ids and units, an enum for an argument that selects behavior, a field valid in one state inside that enum variant, `Option` for absence and never a sentinel. Give a function a new type before it reaches 8 arguments.
- Never clone, copy or allocate to satisfy the borrow checker; rearchitect, even with a big refactoring. Fix a borrow conflict in this order: a shorter borrow or a named `let`; `mem::take`; split the struct into parts and move the method onto the part; indices into an owning `Vec`.
- Several maps with one key become one map of a struct.
- Items are private, then `pub(crate)`; `pub` is the crate API only. A `RefCell` borrow or a lock lasts a few lines and never spans a callback.
- Names: idiomatic Rust names; no `get_` prefix; `as_` is free, `to_` costs, `into_` consumes; no `Manager`, `Helper` or `Util` types; no module name repeated in a type name. A trait exists for generic or `dyn` use, never to group methods; a function with a clear receiver is a method.
- `let _guard = x` keeps a guard alive; `let _ = x` drops it at once.
- A `//` comment states a reason the code cannot show, never history.
- A refactor keeps behavior identical; untested behavior gets a test first.
- `thiserror` in libraries, `anyhow` in applications; a specific `Result` error for failure, `Option` for absence.
- Use `cargo add` for dependencies and `cargo upgrade` for upgrades.
- Suppress a lint with `#[expect]`, which fails once nothing needs it; use `#[allow]` only where the lint fires in some build configurations and not in others.
- Do not hardcode configuration.
- Use plain, direct explanations about current behavior; omit historical or transitional labels and conditional offers to fix known problems.

## Performance rules

- Design data-oriented; read the hot path and its allocations per iteration before changing it.
- Do not allocate on hot paths: the frame loop, the encoder, the network path and the client's render path.
- No sleep or blocking wait in the frame loop.
- Every pacing or heuristic branch must be reachable on a real device and covered by a test of that state.
- Bound the queues and drop stale frames.
- When moving work between CPU, GPU or threads, measure the unit that bounds that device.
- Measure on the headset and on the Mac with the real stream. Alternate A and B, read the result after each pair, and stop as soon as it clearly shows the answer.
- A tracking issue holds decisions and current state, one number for each claim. Run logs and tables of readings stay in evidence files.

## Workflows

### Git and CI

- Before diagnosing a red test, fetch `origin main` and rebase; confirm claimed fixes are ancestors of `HEAD`.
- After a push, watch the CI run of the PR and act on each result.
- Keep related fixes in one PR; finish requested code changes before optional measurements or PR prose.
- Before opening a PR, review the diff for renamed copies and code in the wrong module.
- `cargo test --workspace` and clippy stay green. The client builds with `scripts/build-client.sh`.
- Run each `rm` as a standalone command, then verify separately; never chain it with another operation or loop body.
- Preserve command failures and stderr; a pipeline's final command or an absent error message does not prove success.

### Documentation

- `README.md` is a short guide for users: what cranvr does, how to install it and how to use it. It holds no history and no measurements.
- The details for developers live in `docs/`. `docs/README.md` lists which file holds each section. Update the list when you add, move or rename a section.

### Release

- Release: a bare `v*` tag at green main, never a hand bump. The tag sets the version. `.github/workflows/release.yml` builds the Mac app and the headset APK and publishes them as a GitHub release: [runbook](docs/release.md).

## Simple speech

Never leave prepositions trailing at the ends of clauses (e.g., use "the version by which..." instead of "the version... by"), and keep modifiers close to the words they modify. Use simple words in their original meaning. no poetic, no jargon. No contrastive sentences. No special symbols. No Trailing Participial Phrases. NO for any of these: Overused Buzzwords, Empty Transition Openers, unnecessary adjectives that try to sell an ordinary fact, list things in triples (e.g., "fast, efficient, and reliable" or "streamline, optimize, and scale")., Not only... but also..., wrap-up summaries that don't add actual data. | **No sales pitch or hype:** Adopt a neutral, engineering-first tone. Never use marketing fluff, exclamation points for enthusiasm, or words designed to "sell" a feature (e.g., *effortless, supercharge, magical, lightning-fast*). State facts and mechanics directly.

Never connect sentences with ", and". If you want to add something to the current sentence, then make it the next sentence, not a part connected with ", and bla bla bla". That is a bad antipattern.

Never use the verb-noun pair, like "a home Jack built" or "the dots Jack connects". That is a perversion. Use the correct order of words: "a home built by Jack", "the dots connected by Jack".

Write every sentence in canonical Subject-Verb-Object order. Put the subject first. Put the verb right after the subject. Put the object after the verb: "The compiler rejects the call." Never put the object, an adverb or a complement before the subject, like "Rejected by the compiler is the call" or "Into the cache goes the entry". Never invert the subject and the verb. Put no long insertion between the subject and the verb.
