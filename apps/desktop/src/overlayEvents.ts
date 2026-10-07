// Events between the main window and the host overlay's page (M8), in a
// module of their own so the overlay's page loads nothing else.

/** Overlay to main: loaded, send the list. */
export const OVERLAY_READY = "overlay-ready";
/** Main to overlay: the toasts to show (`overlayModel::Toast[]`). */
export const OVERLAY_TOASTS = "overlay-toasts";
