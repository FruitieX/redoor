import { useAtom } from "jotai";
import { Rows2, WholeWord } from "lucide-react";
import { SegmentedControl } from "#ui/components/segmented-control";
import { inlineDiffChangesAtom } from "#ui/diff-preferences";

/**
 * Switches every diff between inline changes and separate removed and added lines. It sits
 * in each diff view's bar, and all of them share one stored preference, so switching in one
 * view switches the diffs everywhere. On phones it shrinks to its icons, so it fits one bar
 * with the change counts and leaves the screen to the patches.
 */
export function DiffLayoutControl() {
    const [inlineChanges, setInlineChanges] = useAtom(inlineDiffChangesAtom);

    return (
        <SegmentedControl
            label="Diff layout"
            className="w-18 sm:w-64"
            iconOnlyOnPhones
            value={inlineChanges ? "inline" : "separate"}
            onChange={(value) => setInlineChanges(value === "inline")}
            options={[
                {
                    value: "inline",
                    label: "Inline changes",
                    icon: WholeWord,
                    tooltip:
                        "Show an edited line as one row that marks the changed words",
                },
                {
                    value: "separate",
                    label: "Separate lines",
                    icon: Rows2,
                    tooltip: "Show every removed and added line as its own row",
                },
            ]}
        />
    );
}
