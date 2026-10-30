<!--
Lessons learned from earlier dev-split runs (gitagent learn). Each lesson is
general advice drawn only from gitagent's own trajectories and pass/fail
outcomes: no task names, paths, values or answers. Changes are reviewed and
committed like any other prompt change; held-out runs use the committed file.
This comment is removed when the prompt is loaded.
-->
- Order your endgame: finish edits and stop jobs you started, then run the final verification as its own plain command after the last write, not chained with cleanup, edits or guard tests (test -s, command -v ...). Prefer the task's stated check run verbatim and unpiped; it must write nothing.
- Under a tight time budget, start slow work (installs, builds, a timed reference run of the original) in the background at once, logging output and taking stdin from /dev/null, and keep developing meanwhile. Install only what is needed, and poll the log and job state instead of waiting blindly.
- Make the final check exit 0 only when the result is correct: print OK from inside the check (or chain with &&), never as a separate trailing line. In expect scripts a timed-out expect continues silently, so add a timeout branch that exits nonzero.
- After writing a result file, reload it in a separate fresh process and recompute every required property straight from the stated definitions, such as size, type, validity and target values, rather than trusting the variables that produced it.
- Run the task's exact commands verbatim from a neutral directory and from the project folder, since paths resolve relative to the cwd. If your deliverable misses inputs from elsewhere, also look next to itself. A provided program that only works from its own folder is its convention, not your bug.
- Before declaring completion, stop background processes you started that the result does not need. Leave files in the task's directories in place; clear only your own scratch files in /tmp, and only if they could interfere. If ps/pgrep/pkill are missing, read /proc/*/cmdline to find, confirm or kill processes by command line.
- Before building or running anything that might write over provided input files, back them up and work in a scratch directory, then reproduce the stated acceptance check locally (the exact build command, the exact size or format limits, and the scoring metric against the reference) before declaring completion.
- When a hidden harness will call your function or run your deliverable, write your own driver that invokes it the same way across many random inputs plus edge cases (zero, extremes, boundaries), compares against a simple reference, and times it, before declaring completion.
- If a runtime or tool needed to run or check the deliverable is missing, install it (the distro package for a library also pulls in its interpreter) and run every required invocation for real, rather than shipping something untested. Note any version mismatch in your summary.
- Run the final verification as a command whose own text shows the check (a named script or a one-liner, not a heredoc piped into an interpreter). Give it a long enough wait to finish in that same call, since a result collected later by polling may not count as verifying your last change.
