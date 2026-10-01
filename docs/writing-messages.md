# Message Standard — lucidlint finding messages

A finding is one item in the tool's report. The finding's message is a
short instruction the reader acts on, plus the reason that lets the reader
check it. Every rule in the Clarity section of docs/writing-documentation.md
applies to message text. This standard adds the rules specific to messages.

## Structure

- State the mechanism of harm, then the action. Write what breaks while
  the code stays as it is; then write what the reader changes. A message
  without the mechanism is a verdict the reader cannot check.
- Separate the fact from the command. Write the fact in its own sentence.
  Write the command in its own sentence.
- End with the action. Write the last sentence as the command that states
  the object and the change.
- Name the referents. Write the file, the function, and the parameter the
  message is about, using the names at the cited location. Do not write
  "the service", "the function", or "the parameter" as stand-ins.
- Pass every referent the message names as a named capture. Write {rel},
  {function}, {param}, or {value} in the template for each value the
  message names. Do not hard-code a value the finding already carries, and
  do not write a demonstrative — "this", "that", "it" — where the named
  capture fits.
- Write "if" for a condition the reader tests; write "when" only for time.
- Give every classification an action. When you write what a thing is,
  write what the reader does about it in the same place.
- State only what the detector knows. Do not name a class or a method the
  tool has not resolved.

## Reasoning — the reader can check the message

- Write the mechanism so a reader can verify it against the code at the
  cited line. This is the reader's means to decide whether a conflicting
  repo standard is wrong: the tool cannot see the repo's standards.
- State the rule's own exclusions where they apply. Write what is not a
  violation when the rule defines it. Never invent an exemption in a
  message: an exemption the rule does not state is a courtesy the message
  has no authority to give.
- Separate the excluded case from the rule with a test that only the
  excluded case passes. A trait the rule and the exclusion share cannot
  mark the line.
- Write the reason as what the reader will understand. Say what the code
  leaves unstated about the domain, and how the fix states it. The reader
  uses this to judge a repo standard that contradicts the rule.

## Vocabulary

- Use plain words throughout. Report-level words — finding, stamp,
  directive, seam, signal, family, baseline, action — are not
  instructional words (the plain-language requirement, PRD R31). Define
  one in plain words at first use, or write the sentence without it.
- Use the domain's own name for the thing. Write the class, the parameter,
  or the module by its name.

## Concision

- Write the smallest message that states the mechanism and the action.
  Each clause the message does not need makes a re-read likelier.
- Keep every sentence under 30 words.
- Cut verdict phrases that name no mechanism. Write the mechanism itself.

## What a message must not do

- Never prescribe an action that creates another finding. Check the
  prescribed fix against the other rule families before you write it. A
  module-level constant is one such action: the name itself is a
  global-state finding.
- Do not write line numbers or fix commands into the prose. The tool owns
  coordinates; the fix command is structured data, appended as the
  directive tail (R27).
- Do not label a fix mechanical when the shape requires judgment. When the
  shape's cases call for different actions, write each case's action, or
  write the distinction that tells the cases apart.
- Do not write a single action when a sibling case breaks under it. Write
  the decision the reader must make with the actions it leads to.
- Never approve duplicate code. If the shape is a duplicate, write the
  action that removes the duplication.
- Do not name a value at module scope in a fix; the name is a
  global-state finding. Name the value on the class that owns the
  computation.

## Checklist

- [ ] The message names the file, function, and parameter it is about
- [ ] Every varying referent is a named capture, never a hard-coded literal or a demonstrative
- [ ] The mechanism of harm appears before the action
- [ ] A reader can verify the mechanism at the cited line
- [ ] The message ends with the command
- [ ] No report-level word is used as an instruction
- [ ] Every sentence is under 30 words
- [ ] No exemption the rule does not state
- [ ] Conditions use "if"; "when" is for time only
- [ ] Every classification names the action for its case
- [ ] The message asserts only what the detector knows
- [ ] The reason states what the code leaves unstated and how the fix states it
- [ ] The prescribed action creates no other finding
- [ ] No action leaves or approves duplicate code
- [ ] Named values point at the owning class, never module scope