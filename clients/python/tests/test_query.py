"""Runs the Python client against the gitagent binary (GITAGENT_BIN) with a
scripted model, so no API key is needed."""

import os
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from gitagent import GitagentError, query, tool  # noqa: E402

BIN = os.environ.get("GITAGENT_BIN") or str(
    pathlib.Path(__file__).resolve().parents[3] / "target" / "debug" / "gitagent"
)


def write(path: pathlib.Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


class QueryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.tmp.name)
        self.agent = root / "agent"
        write(self.agent / "agent.yaml", 'spec_version: "0.1.0"\nname: lyra\nversion: 1.0.0\ndescription: A test agent.\n')
        write(self.agent / "SOUL.md", "I am Lyra.\n")
        self.env = {**os.environ, "HOME": self.tmp.name}
        os.environ["HOME"] = self.tmp.name

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def script(self, name: str, text: str) -> str:
        path = pathlib.Path(self.tmp.name) / name
        path.write_text(text)
        return f"scripted:{path}"

    def test_turns_tools_hooks_and_resume(self) -> None:
        model = self.script(
            "s.yaml",
            "- tool_calls: [{name: exec, arguments: {cmd: \"echo hi > hi.txt\"}}]\n"
            "- text: \"Wrote hi.txt.\"\n"
            "- tool_calls: [{name: lookup_order, arguments: {id: \"42\"}}]\n"
            "- text: \"Order 42 shipped on Monday.\"\n",
        )
        seen = []

        def pre_tool_use(ctx):
            seen.append(ctx["toolName"])
            return {"action": "allow"}

        def lookup(args):
            return f"Order {args['id']} shipped on Monday."

        q = query(
            ["write a file", "where is order 42?"],
            dir=str(self.agent),
            model=model,
            session_id="t1",
            resume=True,
            binary=BIN,
            hooks={"pre_tool_use": pre_tool_use},
            tools=[tool("lookup_order", "Looks up an order.", {"properties": {"id": {"type": "string"}}}, lookup)],
            out_dir=str(pathlib.Path(self.tmp.name) / "out"),
        )
        with q:
            messages = list(q)
        kinds = [(m["type"], m.get("subtype")) for m in messages]
        self.assertEqual(kinds[0], ("system", "ready"))
        self.assertEqual(kinds[1], ("system", "session_start"))
        self.assertEqual(kinds[-1], ("system", "session_end"))
        self.assertEqual(kinds.count(("system", "turn_end")), 2)
        self.assertEqual(q.session_id(), "t1")
        self.assertEqual(q.manifest()["name"], "lyra")
        self.assertIn("lookup_order", q.tools())
        self.assertEqual(seen, ["exec", "lookup_order"])
        results = [m for m in messages if m["type"] == "tool_result"]
        self.assertEqual(results[1]["content"], "Order 42 shipped on Monday.")
        self.assertEqual(q.result()["result"], "Order 42 shipped on Monday.")
        self.assertEqual(q.costs()["requests"], 4)
        self.assertTrue((self.agent / "hi.txt").is_file())
        self.assertTrue((self.agent / ".gitagent/sessions/t1/messages.json").is_file())
        self.assertEqual(q.wait(), 0)

    def test_send_steer_and_approval(self) -> None:
        write(
            self.agent / "agent.yaml",
            'name: guarded\nversion: 1.0.0\ndescription: Guarded.\ncompliance:\n  risk_tier: high\n  supervision: {human_in_the_loop: always}\n',
        )
        model = self.script(
            "s.yaml",
            "- tool_calls: [{name: exec, arguments: {cmd: \"echo one > one.txt\"}}]\n"
            "- text: \"done one\"\n"
            "- tool_calls: [{name: exec, arguments: {cmd: \"echo two > two.txt\"}}]\n"
            "- text: \"done two\"\n",
        )
        asked = []

        def approver(request):
            asked.append(request["toolName"])
            return (request["args"]["cmd"].endswith("one.txt"), "only one")

        q = query("first", dir=str(self.agent), model=model, approver=approver, binary=BIN,
                  out_dir=str(pathlib.Path(self.tmp.name) / "out"))
        replies = []
        for m in q:
            if m["type"] == "assistant" and m["content"]:
                replies.append(m["content"])
            if m.get("subtype") == "turn_end" and len(replies) == 1:
                q.send("second")
        self.assertEqual(replies, ["done one", "done two"])
        self.assertEqual(asked, ["exec", "exec"])
        self.assertTrue((self.agent / "one.txt").is_file())
        self.assertFalse((self.agent / "two.txt").exists())
        q.close()

    def test_a_bad_start_raises(self) -> None:
        with self.assertRaises(GitagentError) as caught:
            query("x", dir=str(self.agent), model="nonsense", binary=BIN)
        self.assertIn("model", str(caught.exception).lower())
        with self.assertRaises(GitagentError):
            query("x", binary="/nonexistent/gitagent")


if __name__ == "__main__":
    unittest.main()
