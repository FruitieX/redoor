import { useAtom } from "jotai";
import {
    FoldVertical,
    Rows2,
    UnfoldVertical,
    WholeWord,
    WrapText,
} from "lucide-react";
import { SegmentedControl } from "#ui/components/segmented-control";
import { ToggleButton } from "#ui/components/toggle-button";
import { inlineDiffChangesAtom } from "#ui/diff-preferences";
import { cn } from "#ui/utils/cn";
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
 * Switches a diff between wrapping and sideways-scrolling long lines. The single-file views
 * switch the stored preference with it, and diff lists put one in every file's header, so
 * only the file with long lines has to wrap while the others keep their columns aligned.
 */
export function WrapDiffLinesToggle(props: {
    label: string;
    wrap: boolean;
    onChange: (wrap: boolean) => void;
    /** Sizes the toggle to the controls beside it, such as a file header's compact switch. */
    className?: string;
}) {
    return (
        <ToggleButton
            label={props.label}
            pressed={props.wrap}
            tooltip={
                props.wrap
                    ? "Scroll long lines sideways instead of wrapping them"
                    : "Wrap long lines to fit the view"
            }
            className={cn("h-8 w-8 shrink-0 p-0", props.className)}
            onClick={() => props.onChange(!props.wrap)}
        >
            <WrapText className="h-3.5 w-3.5" aria-hidden="true" />
        </ToggleButton>
    );
}

/**
 * Switches one file's diff between its changes with a little context and the whole file with
 * the changes marked in it. It sits beside the file's name, since reading a file whole is a
 * choice for that file, and it never fetches, because the patch already carries the file.
 */
export function WholeFileToggle(props: {
    label: string;
    shown: boolean;
    onChange: (shown: boolean) => void;
    /** Sizes the toggle to the controls beside it, such as a file header's compact switch. */
    className?: string;
}) {
    return (
        <ToggleButton
            label={props.label}
            pressed={props.shown}
            tooltip={
                props.shown
                    ? "Fold the unchanged lines around the changes again"
                    : "Show the whole file with its changes"
            }
            className={cn("h-8 w-8 shrink-0 p-0", props.className)}
            onClick={() => props.onChange(!props.shown)}
        >
            {props.shown ? (
                <FoldVertical className="h-3.5 w-3.5" aria-hidden="true" />
            ) : (
                <UnfoldVertical className="h-3.5 w-3.5" aria-hidden="true" />
            )}
        </ToggleButton>
    );
}

/**
 * The layout and wrap controls of a view that shows a single diff. Its switches set the
 * stored preferences, which are also what every file of a diff list starts with. On
 * phones it shrinks to its icons, so it fits one bar with the change counts and leaves the
 * screen to the patch.
 */
export function DiffLayoutControl() {
    const [inlineChanges, setInlineChanges] = useAtom(inlineDiffChangesAtom);
    const [userState, setUserState] = useUserState();

    return (
        <div className="flex shrink-0 items-center gap-2">
            <DiffLayoutSwitch
                label="Diff layout"
                className="w-18 sm:w-64"
                inline={inlineChanges}
                onChange={setInlineChanges}
            />
            <WrapDiffLinesToggle
                label="Wrap diff lines"
                wrap={userState.wrapDiffLines}
                onChange={(wrap) => {
                    setUserState((current) => ({
                        ...current,
                        wrapDiffLines: wrap,
                    }));
                }}
            />
        </div>
    );
}
