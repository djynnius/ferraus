#!/usr/bin/env python3
"""A small agent loop: Claude edits audio by calling the ferraus CLI as tools.

    pip install anthropic
    export ANTHROPIC_API_KEY=...
    python3 examples/agents/claude_tool_use.py --samples samples [--workdir DIR] [--task "..."]
    python3 examples/agents/claude_tool_use.py --print-tools     # no API call, no SDK needed

Each tool definition is built from `ferraus schema <command>`, so names, flags, choices, and
defaults always match the installed binary. Tool calls run as `ferraus <command> ... --json`
in --workdir, and the JSON envelope goes back to the model as the tool result. Outputs must be
relative paths inside --workdir; nothing is overwritten (the model reads the error envelope
and picks another name). Models are never downloaded: there is no -y.

The model is claude-opus-5-5 unless --model or $ANTHROPIC_MODEL says otherwise.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile

FERRAUS = os.environ.get("FERRAUS", "ferraus")
COMMANDS = ["info", "analyze problems", "denoise", "fx", "normalize", "master"]
DEFAULT_TASK = (
    "Clean up {samples}/podcast/ep02.wav for publishing as a podcast: inspect it, fix what you "
    "find, and bring it to the podcast loudness target for its channel count. Write the result "
    "as ep02.clean.wav and tell me what you did and what to listen for."
)
SYSTEM = (
    "You edit audio with the ferraus tools. Inspect before changing anything, prefer one fx "
    "chain over several passes, read metrics and warnings in every result, and check your work. "
    "Output paths are relative file names in the working directory."
)


def strip_meta(node):
    """Drop $schema/$id and ferraus's x- annotations, which a tool schema does not need."""
    if isinstance(node, dict):
        return {k: strip_meta(v) for k, v in node.items() if not k.startswith(("$", "x-"))}
    if isinstance(node, list):
        return [strip_meta(v) for v in node]
    return node


def build_tools():
    """One tool per command, from its JSON Schema. Returns (tools, name -> (command, schema))."""
    tools, index = [], {}
    for command in COMMANDS:
        spec = json.loads(subprocess.run([FERRAUS, "schema", command], capture_output=True, text=True, check=True).stdout)
        schema = strip_meta(spec["arguments"])
        name = "ferraus_" + command.replace(" ", "_").replace("-", "_")  # same names as `ferraus mcp serve`
        description = (
            f"{spec['summary']}. Runs `ferraus {command} ... --json` and returns its JSON envelope "
            f"(ok, outputs, params, metrics, warnings, result; error with code and hint on failure). "
            f"Arguments are the command's flags with _ for -, plus inputs (paths) and output."
        )
        tools.append({"name": name, "description": description, "input_schema": schema})
        index[name] = (command, schema)
    return tools, index


def to_argv(command, schema, args, workdir):
    """Turn tool input into a ferraus command line. Raises ValueError for unsafe output paths."""
    argv = [FERRAUS, *command.split(), *args.get("inputs", [])]
    for key, value in args.items():
        if key == "inputs":
            continue
        if key == "output":
            path = os.path.normpath(os.path.join(workdir, value))
            if os.path.isabs(value) or not path.startswith(os.path.abspath(workdir) + os.sep):
                raise ValueError(f"output must be a relative path inside the working directory, not {value!r}")
            argv += ["-o", path]
            continue
        flag = "--" + key.replace("_", "-")
        prop = schema["properties"].get(key, {})
        if value is True:
            argv.append(flag)
        elif value is False or value is None:
            continue
        elif isinstance(value, list):
            # A repeatable flag (oneOf string|array, e.g. fx) repeats; a list-valued flag
            # (gains: 0dB,-3dB) takes one comma-joined value.
            if any(alt.get("type") == "array" for alt in prop.get("oneOf", [])):
                for v in value:
                    argv += [flag, str(v)]
            else:
                argv += [flag, ",".join(map(str, value))]
        else:
            argv += [flag, str(value)]
    return argv + ["--json"]


def run_tool(index, name, args, workdir):
    """Run one tool call; returns (text for the tool_result, is_error)."""
    if name not in index:
        return f"unknown tool {name}", True
    command, schema = index[name]
    try:
        argv = to_argv(command, schema, args, workdir)
    except ValueError as e:
        print(f"   refused {name}: {e}")
        return str(e), True
    print(f"   $ {' '.join(argv[1:])}")
    proc = subprocess.run(argv, capture_output=True, text=True, cwd=workdir)
    try:
        envelope = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return f"ferraus exited {proc.returncode} without a JSON envelope: {proc.stderr[-500:]}", True
    status = "ok" if envelope.get("ok") else f"error {envelope.get('error', {}).get('code')}"
    print(f"     -> exit {proc.returncode}, {status}")
    return json.dumps(envelope), not envelope.get("ok")


def agent(task, model, workdir, max_turns):
    import anthropic  # only the API loop needs the SDK

    tools, index = build_tools()
    client = anthropic.Anthropic()  # reads ANTHROPIC_API_KEY
    messages = [{"role": "user", "content": task}]
    for _ in range(max_turns):
        response = client.messages.create(
            model=model, max_tokens=16000, system=SYSTEM, tools=tools, messages=messages
        )
        # Keep the whole content (thinking and tool_use blocks too), unchanged.
        messages.append({"role": "assistant", "content": response.content})
        for block in response.content:
            if block.type == "text" and block.text.strip():
                print(f"\nclaude: {block.text.strip()}\n")
        if response.stop_reason == "refusal":
            print(f"the model declined: {response.stop_details}")
            return 1
        if response.stop_reason != "tool_use":
            return 0 if response.stop_reason == "end_turn" else 1
        # Every tool_use block gets a tool_result, all in one user message.
        results = []
        for block in response.content:
            if block.type == "tool_use":
                text, is_error = run_tool(index, block.name, block.input, workdir)
                results.append({"type": "tool_result", "tool_use_id": block.id, "content": text, "is_error": is_error})
        messages.append({"role": "user", "content": results})
    print(f"stopped after {max_turns} turns")
    return 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--print-tools", action="store_true", help="print the tool definitions and exit")
    ap.add_argument("--samples", default="samples", help="folder made by examples/make-samples.sh")
    ap.add_argument("--workdir", help="where the agent's outputs go (default: a new temp dir)")
    ap.add_argument("--task", help="what to ask for (default: clean up a podcast episode)")
    ap.add_argument("--model", default=os.environ.get("ANTHROPIC_MODEL", "claude-opus-5-5"))
    ap.add_argument("--max-turns", type=int, default=20)
    args = ap.parse_args()

    if args.print_tools:
        tools, _ = build_tools()
        print(json.dumps(tools, indent=2))
        return 0
    workdir = os.path.abspath(args.workdir or tempfile.mkdtemp(prefix="ferraus-agent."))
    os.makedirs(workdir, exist_ok=True)
    task = args.task or DEFAULT_TASK.format(samples=os.path.abspath(args.samples))
    print(f"model {args.model}, working directory {workdir}\ntask: {task}")
    return agent(task, args.model, workdir, args.max_turns)


if __name__ == "__main__":
    sys.exit(main())
