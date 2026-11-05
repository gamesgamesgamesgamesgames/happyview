export interface CollectionStat {
  collection: string;
  count: number;
  indexed: boolean;
}

export interface StatsResponse {
  total_records: number;
  collections: CollectionStat[];
}
