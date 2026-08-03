// Which workspace section the sidebar highlights while the page scrolls.
//
// The three views are one long scrolling document rather than swapped panes, so the active one is
// whichever section has passed a marker line 28% down the viewport — capped so a tall window cannot
// push that line past a short section.
//
// Kept apart from the component because the geometry it reads (`offsetTop`, `clientHeight`) forces a
// synchronous layout: the caller measures on layout changes and calls this per scroll event, and this
// is where the semantics of that split are pinned down.

export type ViewId = "monitor" | "routing" | "processing";
export const VIEWS = ["monitor", "routing", "processing"] as const;

const MARKER_VIEWPORT_FRACTION = 0.28;
const MARKER_MAXIMUM_PX = 180;

export function markerOffset(containerHeight: number) {
  return Math.min(containerHeight * MARKER_VIEWPORT_FRACTION, MARKER_MAXIMUM_PX);
}

/**
 * `sectionTops` is indexed by `VIEWS`. An entry that is not a number — a section missing from the
 * DOM, or geometry not measured yet — never matches, leaving the first view selected. The last
 * section past the marker wins, because the sections are laid out in order.
 */
export function activeViewAt(
  scrollTop: number,
  offset: number,
  sectionTops: readonly number[],
): ViewId {
  const marker = scrollTop + offset;
  let active: ViewId = VIEWS[0];
  VIEWS.forEach((view, index) => {
    if (sectionTops[index] <= marker) active = view;
  });
  return active;
}
