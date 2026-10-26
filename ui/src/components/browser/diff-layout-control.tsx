import { useAtom } from "jotai";
import { Rows2, WholeWord, WrapText } from "lucide-react";
import { SegmentedControl } from "#ui/components/segmented-control";
import { ToggleButton } from "#ui/components/toggle-button";
import { inlineDiffChangesAtom } from "#ui/diff-preferences";
import { useUserState } from "#ui/user-state";

/**
 * Switches every diff between inline changes and separate removed and added lines, and
 * between wrapping and sideways-scrolling long lines. It sits in each diff view's bar, and
 * all of them share the same stored preferences, so switching in one view switches the
 * diffs everywhere. On phones it shrinks to its icons, so it fits one bar with the change
 * counts and leaves the screen to the patches.
 */
export function DiffLayoutControl() {
    const [inlineChanges, setInlineChanges] = useAtom(inlineDiffChangesAtom);
    const [userState, setUserState] = useUserState();

    return (
        <div className="flex shrink-0 items-center gap-2">
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
                        tooltip:
                            "Show every removed and added line as its own row",
                    },
                ]}
            />
            <ToggleButton
                label="Wrap diff lines"
                pressed={userState.wrapDiffLines}
                tooltip={
                    userState.wrapDiffLines
                        ? "Scroll long lines sideways instead of wrapping them"
                        : "Wrap long lines to fit the view"
                }
                className="h-8 w-8 p-0"
                onClick={() => {
                    setUserState((current) => ({
                        ...current,
                        wrapDiffLines: !current.wrapDiffLines,
                    }));
                }}
            >
                <WrapText className="h-3.5 w-3.5" aria-hidden="true" />
            </ToggleButton>
        </div>
    );
}
