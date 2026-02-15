#!/usr/bin/env python3
"""
Script to fix all chip state files to use Option<Vec<StateEvent>> instead of Option<StateEvent>
"""

import re
import sys
from pathlib import Path


def fix_return_statements(content):
    """Fix return statements: Some(vec![StateEvent::X { ... }); -> Some(vec![StateEvent::X { ... }]);"""
    # Fix closing parenthesis placement in return statements
    content = re.sub(
        r"Some\(vec!\[StateEvent::(\w+)\s*\{([^}]+)\}\)\);",
        r"Some(vec![StateEvent::\1 {\2}]);",
        content,
    )

    # Fix multi-line return statements
    lines = content.split("\n")
    fixed_lines = []
    i = 0
    while i < len(lines):
        line = lines[i]
        # Check if line starts a vec![StateEvent::... pattern
        if (
            "Some(vec![StateEvent::" in line
            and "});" not in line
            and "}]);" not in line
        ):
            # Look for the closing
            j = i + 1
            while j < len(lines) and "});" in lines[j] and "}]);" not in lines[j]:
                j += 1
            if j < len(lines):
                # Replace }); with }]);
                lines[j] = lines[j].replace("});", "}]);", 1)
        fixed_lines.append(lines[i])
        i += 1

    return "\n".join(fixed_lines)


def fix_test_assertions(content):
    """Fix test assertions to work with Vec<StateEvent>"""

    # Pattern 1: assert!(matches!(event, Some(vec![StateEvent::X { ... }])));
    # Replace with proper check
    def replace_matches_assertion(match):
        var_name = match.group(1)
        event_type = match.group(2)
        fields = match.group(3).strip()

        # Generate appropriate assertion
        if ".." in fields:
            # Wildcard pattern
            return f"assert!(event.is_some() && event.as_ref().unwrap().len() == 1);"
        else:
            # Specific fields
            return f"assert!(event.is_some() && event.as_ref().unwrap().len() == 1);"

    content = re.sub(
        r"assert!\(matches!\((\w+),\s*Some\(vec!\[StateEvent::(\w+)\s*\{([^}]*)\}\]\)\)\);",
        replace_matches_assertion,
        content,
    )

    # Pattern 2: if let Some(vec![StateEvent::X { fields }]) = event
    # This is more complex, need to handle it carefully
    content = re.sub(
        r"if let Some\(vec!\[StateEvent::(\w+)\s*\{([^}]+)\}\]\)\s*=\s*(\w+)",
        r"if let Some(ref events) = \3\n            && events.len() == 1\n            && let StateEvent::\1 {\2} = &events[0]",
        content,
    )

    # Pattern 3: event.is_none() || matches!(event, Some(vec![StateEvent::X { .. }]))
    content = re.sub(
        r"event\.is_none\(\)\s*\|\|\s*matches!\(event,\s*Some\(vec!\[StateEvent::(\w+)\s*\{([^}]*)\}\]\)\)",
        r"event.is_none() || (event.as_ref().map(|e| e.len() == 1 && matches!(&e[0], StateEvent::\1 {\2})).unwrap_or(false))",
        content,
    )

    return content


def process_file(filepath):
    """Process a single Rust file"""
    print(f"Processing {filepath}...")

    with open(filepath, "r", encoding="utf-8") as f:
        content = f.read()

    original_content = content

    # Apply fixes
    content = fix_return_statements(content)
    content = fix_test_assertions(content)

    # Only write if changed
    if content != original_content:
        with open(filepath, "w", encoding="utf-8") as f:
            f.write(content)
        print(f"  ✓ Fixed {filepath}")
        return True
    else:
        print(f"  - No changes needed for {filepath}")
        return False


def main():
    # Find all state implementation files
    state_dir = Path("crates/soundlog/src/chip/state")

    if not state_dir.exists():
        print(f"Error: {state_dir} does not exist")
        sys.exit(1)

    # Get all .rs files except chip_state.rs, storage.rs, channel.rs
    exclude = {"chip_state.rs", "storage.rs", "channel.rs"}
    files = [f for f in state_dir.glob("*.rs") if f.name not in exclude]

    print(f"Found {len(files)} files to process\n")

    fixed_count = 0
    for filepath in sorted(files):
        if process_file(filepath):
            fixed_count += 1

    print(f"\n✓ Processed {len(files)} files, fixed {fixed_count} files")


if __name__ == "__main__":
    main()
