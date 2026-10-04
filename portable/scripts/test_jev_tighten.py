#!/usr/bin/env python3
"""Unit tests for jev_tighten's offline policy. Run: python3 -m unittest portable/scripts/test_jev_tighten.py"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import jev_tighten as jt

BODY = "$ ls\nnotes.txt  wc.py\n"


def quiet(i, kind, screen, inputs, out_len=1.5, words=0):
    return {"id": i, "kind": kind, "take": [i, i + 1], "out_len": out_len, "drop": False,
            "words": words, "inputs": inputs, "screen_text": screen, "new_text": ""}


def typing(i):
    return {"id": i, "kind": "typing", "take": [i, i + 1], "out_len": 0.5, "drop": False}


def undo_plan(restored_screen, restored_inputs=("key ctrl+u",), restored_kind="settled"):
    """Screen 2 is the pre-mistake prompt, 4 shows the typo, 6 follows the undo, 8 is the end."""
    return {"segments": [
        quiet(0, "settled", BODY + "$ ", []),
        typing(1),
        quiet(2, "settled", BODY + "$ ", ['type "ls"', "key Return"]),
        typing(3),
        quiet(4, "settled", BODY + "$ pyhton3 wc.py", ['type "pyhton3 wc.py"']),
        typing(5),
        quiet(6, restored_kind, restored_screen, list(restored_inputs)),
        typing(7),
        quiet(8, "end", BODY + "$ python3 wc.py\n20 words", ['type "python3 wc.py"', "key Return"]),
    ]}


def answers(plan, undone=(), score=2.0):
    return {s["id"]: {"read_need": {"score": score}, "undoes_prev": {"noul": 0.95 if s["id"] in undone else 0.05}}
            for s in plan["segments"] if s["kind"] in ("settled", "end")}


def dropped(plan):
    return [s["id"] for s in plan["segments"] if s["drop"]]


class SameScreen(unittest.TestCase):
    def test_identical_after_normalizing_spaces_and_blank_runs(self):
        self.assertTrue(jt.same_screen("a  \n\n\n\nb\n$ ", "a\n\nb\n$\n\n", []))

    def test_only_status_line_differs(self):
        before = "log line 1\nlog line 2\napp.log"
        after = "log line 1\nlog line 2\nDon't use line numbers  (press RETURN)"
        self.assertTrue(jt.same_screen(before, after, ['type "-N"']))

    def test_last_line_showing_typed_text_is_not_restored(self):
        self.assertFalse(jt.same_screen(BODY + "$ ", BODY + "$ python3 wc.py", ["key ctrl+u", 'type "python3 wc.py"']))

    def test_body_difference_is_not_restored(self):
        self.assertFalse(jt.same_screen("a\nb\n$ ", "a\nc\n$ ", []))

    def test_single_line_screens_must_be_identical(self):
        self.assertFalse(jt.same_screen("$ ", "> ", []))


class RestoreRule(unittest.TestCase):
    def test_drops_the_screen_that_restores_the_pre_mistake_screen(self):
        plan = undo_plan(BODY + "$ ")
        records = jt.apply_policy(plan, answers(plan, undone={4}))
        self.assertEqual(dropped(plan), [3, 4, 5, 6])
        self.assertEqual([r["restored_dropped"] for r in records if r["restored_dropped"] is not None], [6])

    def test_no_restore_rule_keeps_it(self):
        plan = undo_plan(BODY + "$ ")
        jt.apply_policy(plan, answers(plan, undone={4}), restore_rule=False)
        self.assertEqual(dropped(plan), [3, 4, 5])

    def test_keeps_a_screen_with_new_content(self):
        plan = undo_plan(BODY + "$ \nwarning: history cleared\n$ ")
        jt.apply_policy(plan, answers(plan, undone={4}))
        self.assertEqual(dropped(plan), [3, 4, 5])

    def test_keeps_a_screen_already_showing_the_retyped_command(self):
        plan = undo_plan(BODY + "$ python3 wc.py", restored_inputs=("key ctrl+u", 'type "python3 wc.py"'))
        jt.apply_policy(plan, answers(plan, undone={4}))
        self.assertEqual(dropped(plan), [3, 4, 5])

    def test_never_drops_the_end_screen(self):
        plan = undo_plan(BODY + "$ ", restored_kind="end")
        plan["segments"] = plan["segments"][:7]
        jt.apply_policy(plan, answers(plan, undone={4}))
        self.assertEqual(dropped(plan), [3, 4, 5])

    def test_nothing_dropped_without_an_undo(self):
        plan = undo_plan(BODY + "$ ")
        jt.apply_policy(plan, answers(plan))
        self.assertEqual(dropped(plan), [])


class HoldScale(unittest.TestCase):
    def test_scales_settled_holds_and_not_the_end(self):
        plan = undo_plan(BODY + "$ ")
        base = jt.hold_for(2.0, 0)
        jt.apply_policy(plan, answers(plan), hold_scale=0.7)
        holds = {s["id"]: s["out_len"] for s in plan["segments"] if "screen_text" in s}
        self.assertEqual(holds, {0: base * 0.7, 2: base * 0.7, 4: base * 0.7, 6: base * 0.7, 8: jt.END_MIN})

    def test_end_keeps_a_long_hold_unscaled(self):
        plan = undo_plan(BODY + "$ ")
        plan["segments"][-1]["words"] = 40
        jt.apply_policy(plan, answers(plan, score=4.0), hold_scale=0.5)
        self.assertEqual(plan["segments"][-1]["out_len"], jt.hold_for(4.0, 40))
        self.assertGreater(jt.hold_for(4.0, 40), jt.END_MIN)

    def test_default_scale_leaves_holds_as_the_table_gives(self):
        plan = undo_plan(BODY + "$ ")
        jt.apply_policy(plan, answers(plan))
        self.assertEqual(plan["segments"][2]["out_len"], jt.hold_for(2.0, 0))


if __name__ == "__main__":
    unittest.main()
