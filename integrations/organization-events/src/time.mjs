// Preserve source micro/nanoseconds at archive boundaries; JavaScript Date
// silently truncates them and could otherwise include an out-of-range fact.
export function timestampNanos(value){
 if(typeof value!=='string')throw Error('INVALID_EXACT_TIMESTAMP');const match=/^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,9}))?(Z|[+-]\d\d:\d\d)$/.exec(value);if(!match)throw Error('INVALID_EXACT_TIMESTAMP');
 const seconds=Date.parse(match[1]+match[3]);if(!Number.isFinite(seconds))throw Error('INVALID_EXACT_TIMESTAMP');return BigInt(seconds)*1000000n+BigInt((match[2]??'').padEnd(9,'0'));
}
export function inExactRange(value,from,through){return timestampNanos(value)>=timestampNanos(from)&&timestampNanos(value)<=timestampNanos(through);}
// Both canonical ClickHouse histories use DateTime64(3,'UTC'). Original source
// precision remains in the company source envelope and exact archive checks.
export function clickhouseMillis(value){return new Date(value).toISOString().replace('T',' ').replace('Z','');}
