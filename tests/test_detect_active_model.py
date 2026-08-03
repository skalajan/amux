"""Unit tests for detect_active_model — the source of the dashboard model badge.

The badge is derived from the session's own JSONL conversation file. The
regression these tests pin: `/model opus` writes only `user`-type entries
(the command echo and its stdout), never an `assistant` entry carrying a
`message.model` field. Scanning backward for the last `message.model` therefore
returns the *previous* model, so the card kept showing SONNET after the user
switched to Opus — indefinitely for an idle session, since nothing else ever
writes a newer assistant turn.

Imported from amux-server.py via importlib, same as test_shell_quote_flags.py,
so no drift is possible.
"""

import importlib.util
import json
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).parent.parent
SERVER_PATH = REPO_ROOT / "amux-server.py"


@pytest.fixture(scope="module")
def amux_server():
    spec = importlib.util.spec_from_file_location("amux_server", SERVER_PATH)
    assert spec is not None and spec.loader is not None, f"could not load {SERVER_PATH}"
    mod = importlib.util.module_from_spec(spec)
    sys.modules["amux_server"] = mod
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture
def conversation(amux_server, tmp_path, monkeypatch):
    """Write JSONL entries to a fake CLAUDE_HOME and return detect_active_model's answer.

    detect_active_model resolves the project dir as CLAUDE_HOME/projects/<slug>,
    so we point CLAUDE_HOME at tmp_path and derive the slug with the production
    _project_name helper.
    """
    monkeypatch.setattr(amux_server, "CLAUDE_HOME", tmp_path)

    def run(entries, work_dir="/Users/someone/Projects/demo", conversation_id="conv-1"):
        # The module-level cache is keyed on (work_dir, conversation_id) and would
        # leak answers between cases inside the 15s TTL.
        amux_server._model_cache.clear()
        proj = tmp_path / "projects" / amux_server._project_name(work_dir)
        proj.mkdir(parents=True, exist_ok=True)
        (proj / f"{conversation_id}.jsonl").write_text(
            "\n".join(json.dumps(e) for e in entries) + "\n"
        )
        return amux_server.detect_active_model(work_dir, conversation_id)

    return run


def _assistant(model):
    return {"type": "assistant", "message": {"model": model, "content": [{"type": "text", "text": "ok"}]}}


def _user(text):
    return {"type": "user", "message": {"role": "user", "content": text}}


def _slash_model(args):
    """The exact shape Claude Code writes for a `/model <args>` invocation."""
    return _user(
        f"<command-name>/model</command-name>"
        f"            <command-message>model</command-message>"
        f"            <command-args>{args}</command-args>"
    )


def _slash_model_stdout(label):
    return _user(f"<local-command-stdout>Set model to \x1b[1m{label}\x1b[22m "
                 f"and saved as your default for new sessions</local-command-stdout>")


# ── The regression case ──────────────────────────────────────────────────────

def test_slash_model_after_last_assistant_turn_wins(conversation):
    """The reported bug: session ran on sonnet, user typed `/model opus`, and no
    assistant turn has happened since. The badge must read opus, not sonnet."""
    assert conversation([
        _user("hello"),
        _assistant("claude-sonnet-5"),
        _slash_model("opus"),
        _slash_model_stdout("Opus 5"),
    ]) == "opus"


def test_slash_model_on_idle_session_with_long_history(conversation):
    """Many assistant turns on the old model do not outvote one later /model."""
    entries = []
    for _ in range(50):
        entries.append(_user("go"))
        entries.append(_assistant("claude-sonnet-5"))
    entries.append(_slash_model("opus"))
    entries.append(_slash_model_stdout("Opus 5"))
    assert conversation(entries) == "opus"


def test_assistant_turn_after_slash_model_wins(conversation):
    """Once the session actually replies, the real model observed on the wire is
    the better answer — it reflects any alias resolution or fallback."""
    assert conversation([
        _assistant("claude-sonnet-5"),
        _slash_model("opus"),
        _assistant("claude-opus-5"),
    ]) == "claude-opus-5"


def test_full_model_id_argument_passes_through(conversation):
    assert conversation([
        _assistant("claude-sonnet-5"),
        _slash_model("claude-opus-5"),
    ]) == "claude-opus-5"


def test_bracketed_megacontext_argument_passes_through(conversation):
    """`--model claude-opus-4-6[1m]` is a supported id elsewhere in the server."""
    assert conversation([
        _assistant("claude-sonnet-5"),
        _slash_model("claude-opus-4-6[1m]"),
    ]) == "claude-opus-4-6[1m]"


# ── /model forms that carry no usable argument ──────────────────────────────

def test_bare_slash_model_falls_back_to_last_assistant(conversation):
    """`/model` with no args opens the interactive picker; the JSONL records no
    id, so the last observed assistant model stays the best available answer."""
    assert conversation([
        _assistant("claude-sonnet-5"),
        _slash_model(""),
    ]) == "claude-sonnet-5"


def test_whitespace_only_argument_falls_back(conversation):
    assert conversation([
        _assistant("claude-sonnet-5"),
        _slash_model("   "),
    ]) == "claude-sonnet-5"


def test_malformed_argument_rejected_and_falls_back(conversation):
    """A shell-metachar payload must never reach the UI as a model name."""
    assert conversation([
        _assistant("claude-sonnet-5"),
        _slash_model("opus; rm -rf /"),
    ]) == "claude-sonnet-5"


def test_other_slash_command_is_not_treated_as_model_switch(conversation):
    assert conversation([
        _assistant("claude-sonnet-5"),
        _user("<command-name>/compact</command-name><command-args>focus on tests</command-args>"),
    ]) == "claude-sonnet-5"


def test_user_text_mentioning_model_command_is_ignored(conversation):
    """Prose that merely talks about /model must not move the badge."""
    assert conversation([
        _assistant("claude-sonnet-5"),
        _user("you should run /model opus to switch"),
    ]) == "claude-sonnet-5"


# ── Pre-existing behaviour that must not regress ────────────────────────────

def test_last_assistant_model_when_no_slash_command(conversation):
    assert conversation([
        _assistant("claude-sonnet-5"),
        _user("hi"),
        _assistant("claude-opus-5"),
    ]) == "claude-opus-5"


def test_empty_conversation_returns_empty(conversation):
    assert conversation([_user("hi")]) == ""


def test_missing_conversation_file_returns_empty(amux_server, tmp_path, monkeypatch):
    monkeypatch.setattr(amux_server, "CLAUDE_HOME", tmp_path)
    amux_server._model_cache.clear()
    work_dir = "/Users/someone/Projects/demo"
    (tmp_path / "projects" / amux_server._project_name(work_dir)).mkdir(parents=True)
    assert amux_server.detect_active_model(work_dir, "no-such-conv") == ""


def test_no_work_dir_returns_empty(amux_server):
    assert amux_server.detect_active_model("") == ""


def test_content_blocks_form_is_handled(conversation):
    """Slash commands are sometimes recorded as a content-block list rather than
    a bare string; both shapes must be read."""
    entry = {"type": "user", "message": {"role": "user", "content": [
        {"type": "text", "text": "<command-name>/model</command-name>"
                                 "<command-args>haiku</command-args>"},
    ]}}
    assert conversation([_assistant("claude-sonnet-5"), entry]) == "haiku"
