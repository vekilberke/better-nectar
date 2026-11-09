declare const __PHONE_LINK__: boolean | undefined;

/** The owner's own build (`bun run build:own`): phone link on, upstream identity, no updater. */
export const PHONE_LINK: boolean = typeof __PHONE_LINK__ !== "undefined" && __PHONE_LINK__ === true;

export const APP_NAME = PHONE_LINK ? "Nectar" : "Better Nectar";
