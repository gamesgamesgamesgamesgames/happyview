export interface CollectionStat {
  collection: string;
  count: number;
}

export interface StatsResponse {
  total_records: number;
  collections: CollectionStat[];
}
