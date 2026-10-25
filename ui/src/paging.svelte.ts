import {preserveViewport} from './liveUpdates';
import {errorText} from './format';

export const PAGE_SIZE = 50;
export type PageResult<Row> = {rows: Row[]; total: number};
type Load<Row> = (offset: number, limit: number, anchor?: bigint) => Promise<PageResult<Row>>;

// Pages count from the newest row. Leaving the first page, or holding, anchors the list
// below its newest key so that rows arriving later do not shift the offsets.
export class Paged<Row> {
 rows = $state<Row[]>([]);
 total = $state(0);
 offset = $state(0);
 error = $state('');
 held = $state(false);
 #anchor: bigint | undefined;
 #generation = 0;
 #busy = false;
 constructor(private readonly load: Load<Row>, readonly limit = PAGE_SIZE, private readonly key?: (row: Row) => bigint) {}
 get pages() { return Math.max(1, Math.ceil(this.total / this.limit)); }
 async refresh() {
  if (this.#busy) return;
  this.#busy = true;
  const generation = this.#generation;
  try {
   const page = await this.load(this.offset, this.limit, this.#anchor);
   if (generation !== this.#generation) return;
   preserveViewport(() => { this.rows = page.rows; this.total = page.total; });
   this.error = '';
   if (this.offset > 0 && this.offset >= page.total) this.go(Math.max(0, this.pages - 1) * this.limit);
  } catch (caught) {
   if (generation === this.#generation) this.error = errorText(caught);
  } finally {
   if (generation === this.#generation) this.#busy = false;
  }
 }
 reset() {
  this.offset = 0;
  if (!this.held) this.#anchor = undefined;
  this.#reload();
 }
 go(offset: number) {
  if (offset > 0 && this.#anchor === undefined) this.#anchor = this.#newest();
  if (offset === 0 && !this.held) this.#anchor = undefined;
  this.offset = offset;
  this.#reload();
 }
 toggleHold() {
  this.held = !this.held;
  if (this.held) this.#anchor ??= this.#newest();
  else { this.#anchor = undefined; this.offset = 0; }
  this.#reload();
 }
 #newest() { return this.key && this.rows.length ? this.key(this.rows[0]) + 1n : undefined; }
 #reload() { this.#generation++; this.#busy = false; void this.refresh(); }
}
