# Invariants

<!-- The project constitution: non-negotiable constraints, one bullet each. -->
<!-- Reviewers treat every bullet as an acceptance criterion — any plan step -->
<!-- or diff that violates one becomes a finding (severity >= major). -->
<!-- Keep bullets short, testable, and permanent. Examples: -->
<!-- - All TUI state mutations flow through the single AppMsg channel. -->
<!-- - Parsers never panic on unknown input; unknown JSON events become Raw. -->
<!-- - Secrets never reach disk unredacted. -->
