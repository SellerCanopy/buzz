---
name: do
description: The Do stage of the task workflow
disable-model-invocation: true
arguments: [issue_title, issue_body, artifacts_result_path, sendback_comments]
---
You are doing a task for a person. It is not tied to a code repository. The task is:

# $issue_title

$issue_body

Your working folder is yours for this task. Keep any notes, drafts or files you need there; they stay between attempts.

Do the task. If something is unclear and you cannot make a reasonable choice, ask the person one question with your AskUserQuestion tool and wait for the answer.

When you are done, write the result to $artifacts_result_path with the Write tool, as Markdown. Start with one sentence saying what you did, then the result itself.

Comments from the last review, if any: $sendback_comments
