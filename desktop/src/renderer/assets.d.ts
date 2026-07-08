// Ambient declarations for static asset imports handled by Vite.
declare module "*.png" {
	const src: string;
	export default src;
}
declare module "*.svg" {
	const src: string;
	export default src;
}
