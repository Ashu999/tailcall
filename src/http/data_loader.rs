use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::async_trait;
use async_graphql::dataloader::{DataLoader, HashMapCache, Loader};
use async_graphql::futures_util::future::join_all;
use async_graphql_value::ConstValue;
use derive_setters::Setters;

use url::Url;

use crate::http::Method;
use crate::http::Response;

use crate::evaluation_context::get_path_value;
use crate::http::HttpClient;
use std::hash::{Hash, Hasher};
use crate::json::JsonLike;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointKey {
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub method: Method,
    pub match_key_value: ConstValue,
    pub match_path: Vec<String>,
    pub batching_enabled: bool,
    pub list: bool,
}

impl Hash for EndpointKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.url.hash(state);
        self.match_key_value.as_str_ok().unwrap_or("").hash(state);
    }
}
#[derive(Default, Setters, Clone)]
#[setters(strip_option)]
pub struct HttpDataLoader {
    client: HttpClient,
    headers: Option<BTreeMap<String, String>>,
}

impl HttpDataLoader {
    pub fn new(client: HttpClient) -> Self {
        HttpDataLoader { client, headers: None }
    }

    pub fn to_async_data_loader(self) -> DataLoader<HttpDataLoader, HashMapCache> {
        DataLoader::with_cache(self, tokio::spawn, HashMapCache::new())
    }

    pub fn get_headers(self) -> BTreeMap<String, String> {
        self.headers.unwrap_or_default()
    }

    pub async fn get_unbatched_results(
        &self,
        keys: &[EndpointKey],
    ) -> async_graphql::Result<
        HashMap<EndpointKey, <HttpDataLoader as Loader<EndpointKey>>::Value>,
        <HttpDataLoader as Loader<EndpointKey>>::Error,
    > {
        let key = &keys[0];
        let res = self
            .client
            .get(key.url.clone(), self.headers.clone().unwrap_or_default())
            .await
            .map_err(|e| anyhow::Error::from(Arc::new(e)));
        let mut map = HashMap::new();
        map.insert(key.clone(), res?);
        Ok(map)

        // let unbatched_keys = keys
        //     .iter()
        //     .filter(|key| !key.batching_enabled)
        //     .map(|key| (*key).clone())
        //     .collect::<Vec<_>>();
        // let futures: Vec<_> = unbatched_keys
        //     .iter()
        //     .map(|key| async {
        //         let result = self
        //             .client
        //             .get(key.url.clone(), self.headers.clone().unwrap_or_default())
        //             .await
        //             .map_err(|e| anyhow::Error::from(Arc::new(e)));
        //         (key.clone(), result)
        //     })
        //     .collect();
        //
        // let results = join_all(futures).await;
        //
        // results.into_iter().map(|(key, result)| Ok((key, result?))).collect()
    }

    pub fn group_by_url_and_type(&self, keys: &[EndpointKey]) -> HashMap<Url, Vec<EndpointKey>> {
        keys.iter()
            .filter(|endpoint_key| endpoint_key.batching_enabled)
            .fold(HashMap::new(), |mut acc, key| {
                let group = acc.entry(key.url.clone()).or_default();
                group.push(key.clone());
                acc
            })
    }

    async fn get_batched_results(
        &self,
        keys: &[EndpointKey],
    ) -> Vec<
        async_graphql::Result<
            HashMap<EndpointKey, <HttpDataLoader as Loader<EndpointKey>>::Value>,
            <HttpDataLoader as Loader<EndpointKey>>::Error,
        >,
    > {
        let batched_key_groups = self.group_by_url_and_type(keys);
        join_all(
            batched_key_groups
                .iter()
                .map(|(url, keys)| self.get_batched_results_for_url(url.clone(), keys)),
        )
        .await
    }

    async fn get_batched_results_for_url(
        &self,
        url: Url,
        keys: &[EndpointKey],
    ) -> async_graphql::Result<
        HashMap<EndpointKey, <HttpDataLoader as Loader<EndpointKey>>::Value>,
        <HttpDataLoader as Loader<EndpointKey>>::Error,
    > {
        let result = self
            .client
            .get(url, self.headers.clone().unwrap_or_default())
            .await
            .map_err(|e| Arc::new(anyhow::Error::from(e)));

        match result {
            Err(e) => Err(e),
            Ok(response) => {
                if let async_graphql::Value::List(list) = &response.body {
                    let mut map: HashMap<
                        EndpointKey,
                        <HttpDataLoader as Loader<EndpointKey>>::Value,
                    > = HashMap::with_capacity(keys.len());

                    for key in keys.iter() {
                        let body = if key.list {
                            async_graphql::Value::List(
                                list.iter()
                                    .filter(|&item| {
                                        get_path_value(item, &key.match_path)
                                            .map_or(false, |value| value == &key.match_key_value)
                                    })
                                    .cloned()
                                    .collect(),
                            )
                        } else {
                            list.iter()
                                .find(|&item| {
                                    get_path_value(item, &key.match_path)
                                        .map_or(false, |value| value == &key.match_key_value)
                                })
                                .cloned()
                                .unwrap_or(ConstValue::Null)
                        };

                        let response_for_key = Response {
                            status: response.status,
                            headers: response.headers.clone(),
                            body,
                            ttl: response.ttl,
                        };
                        map.insert(key.clone(), response_for_key);
                    }
                    Ok(map)
                } else {
                    // TODO: Consider handling the error case instead of returning an empty map
                    Ok(HashMap::new())
                }
            }
        }
    }

}

#[async_trait::async_trait]
impl Loader<EndpointKey> for HttpDataLoader {
    type Value = Response;
    type Error = Arc<anyhow::Error>;

    async fn load(
        &self,
        keys: &[EndpointKey],
    ) -> async_graphql::Result<HashMap<EndpointKey, Self::Value>, Self::Error> {
        //let batched_results = self.get_batched_results(keys).await;
        let mut unbatched_results = self.get_unbatched_results(keys).await?;

        // for result in batched_results {
        //     for (key, value) in result? {
        //         unbatched_results.insert(key, value);
        //     }
        // }
        Ok(unbatched_results)
    }
}