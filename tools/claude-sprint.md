You are executing sprint __ID__ of the fhir-service project at
/user/Project/fhir-service. Be terse in all output; minimal detail.

Sprint: __ID__
Backlog ids: __IDS__
Goal: __GOAL__
Done criteria: __DONE__
Verify: __VERIFY__

== Read the documents first
Read all of these before changing anything; they are authoritative:
doc/Confidential.adoc, doc/Process.adoc, doc/Pilot.adoc, doc/Sprints.adoc,
doc/Tracker.adoc, and every file in doc/adr/.

== Context from memory
Use the memoria MCP: call memoria_search_memories with agent_id "opencode" for
the project, the sprint plan, and sprint __ID__. Append your outcome with
memoria_add_memory (agent_id "opencode", infer false) when you finish.

== Rules that bind (from doc/, all of them apply)
- No code comments anywhere. Documentation comments on public items only.
- Output short and concise.
- Commit messages at most 80 characters, clean and concise; no large commits.
- No brand or application names in docs, code, or commit messages.
- Never mention the machine, its operating system, disk space, network, or
  host-specific paths in docs, code, or messages.
- TDD: a failing test first, then the change. Coverage stays above 85%.
- One commit per backlog id, in this order: __IDS__. Use
  git -c commit.gpgsign=false commit -m "<ID>: <summary>". Do not push, amend,
  rebase, or reset; there is no remote.
- Every change traces to a backlog id; no orphan commits.
- Verify before claiming done: tests pass and behaviour is live-verified.
- Write only inside the repository: no external directories, not even the
  system temp directory. Use scratch/ for temporary artifacts.
- Never stage doc/Confidential.adoc: it is excluded from version control and
  must never enter history.
- Live smoke tests set an explicit read timeout and use HTTP/1.0 or
  Connection: close; keep-alive reads block and fail.
- A test never guesses a free port: the server binds port zero and announces
  the address; the test reads it.
- Rust per the ADRs: crates under crates/, core depends on no adapter, async,
  parse-don't-validate, newtypes for ids, exhaustive matching.
- Backing services come from compose.yaml: docker compose up -d --wait.

== Work
1. Implement only this sprint's ids. Do not start other sprints.
2. Run cargo build --workspace and cargo test --workspace until green. Run
   clippy if it is installed.
3. Update doc/Tracker.adoc History with one line for the sprint.
4. Write doc/review/__ID__.adoc containing: `Status: done` or
   `Status: blocked`, the sprint id, what changed, tests added, verification
   output summary, and blockers.
5. Append the outcome to memoria.
6. Do not edit doc/Pilot.adoc, doc/Sprints.adoc, doc/Process.adoc,
   doc/Confidential.adoc, or tools/.

Even if you cannot finish, write the report and commit what you have: a blocked
report with a clear cause is a valid outcome, silence is not.
