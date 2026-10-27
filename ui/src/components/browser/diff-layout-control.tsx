import { useAtom } from "jotai";
import { Rows2, WholeWord, WrapText } from "lucide-react";
import { SegmentedControl } from "#ui/components/segmented-control";
import { ToggleButton } from "#ui/components/toggle-button";
import { inlineDiffChangesAtom } from "#ui/diff-preferences";
import { useUserState } from "#ui/user-state";

/**
 * Picks between inline changes and separate removed and added lines for one diff. The
 * single-file views switch the stored preference with it, and diff lists put a compact one in
 * every file's header, so each file can be read the way its edits read best.
 */
export function DiffLayoutSwitch(props: {
    label: string;
    inline: boolean;
    onChange: (inline: boolean) => void;
    className?: string;
    /** Shows only the icons, for a switch inside a file header beside the file's name. */
    iconOnly?: boolean;
}) {
    return (
        <SegmentedControl
            label={props.label}
            className={props.className}
            iconOnlyOnPhones
            iconOnly={props.iconOnly}
            value={props.inline ? "inline" : "separate"}
            onChange={(value) => props.onChange(value === "inline")}
            options={[
                {
                    value: "inline",
                    label: "Inline changes",
                    icon: WholeWord,
                    tooltip:
                        "Inline changes: show an edited line as one row that marks the changed words",
                },
                {
                    value: "separate",
                    label: "Separate lines",
                    icon: Rows2,
                    tooltip:
                        "Separate lines: show every removed and added line as its own row",
                },
            ]}
        />
    );
}

/**
 * Switches every diff between wrapping and sideways-scrolling long lines. Wrapping is a
 * matter of screen width rather than of one file's edits, so it stays one user-wide setting
 * in each diff view's bar and switching it in one view switches the diffs everywhere.
 */
export function WrapDiffLinesToggle() {
    const [userState, setUserState] = useUserState();

    return (
        <ToggleButton
            label="Wrap diff lines"
            pressed={userState.wrapDiffLines}
            tooltip={
                userState.wrapDiffLines
                    ? "Scroll long lines sideways instead of wrapping them"
                    : "Wrap long lines to fit the view"
            }
            className="h-8 w-8 shrink-0 p-0"
            onClick={() => {
                setUserState((current) => ({
                    ...current,
                    wrapDiffLines: !current.wrapDiffLines,
                }));
            }}
        >
            <WrapText className="h-3.5 w-3.5" aria-hidden="true" />
        </ToggleButton>
    );
}

/**
 * The layout and wrap controls of a view that shows a single diff. Its layout switch sets
 * the stored preference, which is also what every file of a diff list starts with. On
 * phones it shrinks to its icons, so it fits one bar with the change counts and leaves the
 * screen to the patch.
 */
export function DiffLayoutControl() {
    const [inlineChanges, setInlineChanges] = useAtom(inlineDiffChangesAtom);

    return (
        <div className="flex shrink-0 items-center gap-2">
            <DiffLayoutSwitch
                label="Diff layout"
                className="w-18 sm:w-64"
                inline={inlineChanges}
                onChange={setInlineChanges}
            />
            <WrapDiffLinesToggle />
        </div>
    );
}
