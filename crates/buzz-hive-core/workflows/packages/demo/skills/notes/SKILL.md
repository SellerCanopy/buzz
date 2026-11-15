---
name: notes
description: The Notes stage of the demo workflow
disable-model-invocation: true
arguments: [issue_title, issue_body, artifacts_notes_path, sendback_comments]
---
This is a short demo of Kiln on the repository in your working folder. The issue is:

# $issue_title

$issue_body

Do these steps in order, one at a time:

1. Read one file in the working folder, such as its README, with the Read tool.
2. Run this shell command with the Bash tool: `node -e "console.log(process.version)"`. A person is asked for permission first. If they refuse, say so and carry on.
3. Ask the person one question with your AskUserQuestion tool: which word goes at the top of the notes, with the options Amber and Teal. Wait for the answer.
4. Write the notes to $artifacts_notes_path with the Write tool: the chosen word as a heading, then three short lines about the file you read and what the command printed.

Do not create or change any file in the working folder.

Comments from the last review, if any: $sendback_comments
