/** An entry of the sidebar as the browser gets it: a page, a link out, or a page not written yet. */
export interface NavLink {
	title: string
	/** absent for a page the index names and nobody has written yet */
	href?: string
	external?: boolean
}

export interface NavGroup {
	title: string
	items: NavLink[]
}
