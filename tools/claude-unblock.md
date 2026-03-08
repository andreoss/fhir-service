You are unblocking the fhir-service sprint loop at /user/Project/fhir-service.
Be terse.

The sprint loop stopped on a blocked sprint. Do this and then exit:

1. Read doc/Tracker.adoc (History and Rules), the newest doc/review/<id>.adoc,
   and doc/review/<id>.log to find the cause of the block.
2. Fix the cause: code, test, or tooling. Do not weaken a test to make it pass.
3. Commit each fix with `git -c commit.gpgsign=false commit -m "<ID>: <summary>"`,
   under 80 characters.
4. If the sprint is genuinely unfixable, write doc/review/<id>.adoc with
   `Status: blocked` and the reason, and append the rule to doc/Tracker.adoc
   under Rules (R-xx) per R-05.
5. Append a short outcome to doc/Tracker.adoc History.

Rules:
- Never stage doc/Confidential.adoc: it is excluded from version control.
- Write only inside the repository: no external directories, not even /tmp.
  Use scratch/ for temporary artifacts.
- Do not edit doc/Pilot.adoc, doc/Sprints.adoc, doc/Process.adoc, or tools/.
- Do not start the sprint loop yourself; it is restarted for you.
- Do not ask questions. Make one fix pass, then stop.
