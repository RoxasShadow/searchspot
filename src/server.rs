#![allow(non_upper_case_globals)]
use rustc_serialize::json::{self, ToJson};

use rs_es::Client;

use iron::prelude::*;
use iron::{status, Handler, Headers};
use iron::mime::Mime;

use http_logger::Logger as HTTPLogger;

use router::Router;

use params::*;

use oath::*;

use config::*;
use search::SearchResult;
use resource::Resource;
use logger::Logger;

use std::collections::HashMap;
use std::env;
use std::io::Read;
use std::marker::PhantomData;

macro_rules! try_or_422 {
  ($expr:expr) => (match $expr {
    Ok(val)  => val,
    Err(err) => {
      let error_message = format!("{}", err);

      error!("{}", error_message);

      let mut error = HashMap::new();
      error.insert("error", error_message);

      let content_type = "application/json".parse::<Mime>().unwrap();
      return Ok(Response::with(
        (content_type, status::UnprocessableEntity, json::encode(&error).unwrap())
      ))
    }
  })
}

macro_rules! unauthorized {
  () => ({
    return Ok(Response::with(
      (status::Unauthorized)
    ))
  })
}

macro_rules! authorization {
  ($trait_name:ident, $mode:ident) => {
    trait $trait_name {
      fn is_authorized(&self, auth_config: AuthConfig, headers: &Headers) -> bool {
        if auth_config.enabled == false {
          return true;
        }

        match headers.get_raw("Authorization") {
          Some(header) => match String::from_utf8(header[0].to_owned()) {
            Ok(header) => {
              match header.split("token ").collect::<Vec<&str>>().last() {
                Some(token) => {
                  match token.parse::<u64>() {
                    Ok(token) => totp_raw(auth_config.$mode.as_bytes(), 6, 0, 30) == token,
                    Err(_)    => false,
                  }
                },
                None => false
              }
            },
            Err(_) => false
          },
          None => false
        }
      }
    }
  }
}

#[derive(Clone)]
pub struct Server<R: Resource> {
  config:   Config,
  endpoint: String,
  resource: PhantomData<R>
}

authorization!(ReadableEndpoint, read);
authorization!(WritableEndpoint, write);

pub struct SearchableHandler<R> {
  config:   Config,
  resource: PhantomData<R>
}

impl<R: Resource> SearchableHandler<R> {
  fn new(config: Config) -> Self {
    SearchableHandler::<R> { resource: PhantomData, config: config }
  }
}

impl<R: Resource> ReadableEndpoint for SearchableHandler<R> {}

impl<R: Resource> Handler for SearchableHandler<R> {
  fn handle(&self, req: &mut Request) -> IronResult<Response> {
    if !self.is_authorized(self.config.auth.to_owned(), &req.headers) {
      unauthorized!();
    }

    let mut client = Client::new(&*self.config.es.host, self.config.es.port);

    let params   = try_or_422!(req.get_ref::<Params>());
    let response = SearchResult {
      results: R::search(&mut client, &*self.config.es.index, params),
      params:  params.to_owned()
    };

    let content_type = "application/json".parse::<Mime>().unwrap();
    Ok(Response::with(
      (content_type, status::Ok, try_or_422!(json::encode(&response.to_json())))
    ))
  }
}

pub struct IndexableHandler<R> {
  config:   Config,
  resource: PhantomData<R>
}

impl<R: Resource> IndexableHandler<R> {
  fn new(config: Config) -> Self {
    IndexableHandler::<R> { resource: PhantomData, config: config }
  }
}

impl<R: Resource> WritableEndpoint for IndexableHandler<R> {}

impl<R: Resource> Handler for IndexableHandler<R> {
  fn handle(&self, req: &mut Request) -> IronResult<Response> {
    if !self.is_authorized(self.config.auth.to_owned(), &req.headers) {
      unauthorized!();
    }

    let mut payload = String::new();
    req.body.read_to_string(&mut payload).unwrap();

    let mut client = Client::new(&*self.config.es.host, self.config.es.port);

    let resource: R = try_or_422!(json::decode(&payload));
    try_or_422!(resource.index(&mut client, &*self.config.es.index));

    Ok(Response::with(status::Created))
  }
}

pub struct ResettableHandler<R> {
  config:   Config,
  resource: PhantomData<R>
}

impl<R: Resource> ResettableHandler<R> {
  fn new(config: Config) -> Self {
    ResettableHandler::<R> { resource: PhantomData, config: config }
  }
}

impl<R: Resource> WritableEndpoint for ResettableHandler<R> {}

impl<R: Resource> Handler for ResettableHandler<R> {
  fn handle(&self, req: &mut Request) -> IronResult<Response> {
    if !self.is_authorized(self.config.auth.to_owned(), &req.headers) {
      unauthorized!();
    }

    let mut client = Client::new(&*self.config.es.host, self.config.es.port);
    match R::reset_index(&mut client, &*self.config.es.index) {
      Ok(_)  => Ok(Response::with(status::NoContent)),
      Err(_) => Ok(Response::with(status::UnprocessableEntity))
    }
  }
}

impl<R: Resource> Server<R> {
  pub fn new(endpoint: &str) -> Self {
    let config = match env::args().nth(1) {
      Some(file) => Config::from_file(file),
      None       => Config::from_env()
    };

    Server {
      config:   config,
      endpoint: endpoint.to_owned(),
      resource: PhantomData
    }
  }

  pub fn start(&self) {
    Logger::init().unwrap();

    let host = format!("{}:{}", self.config.http.host, self.config.http.port);

    println!("Searchspot v{}\n{}\n{}\n", env!("CARGO_PKG_VERSION"),
                                         self.config.es,
                                         self.config.http);

    let mut router = Router::new();
    router.get(&self.endpoint,    SearchableHandler::<R>::new(self.config.to_owned()));
    router.post(&self.endpoint,   IndexableHandler::<R>::new(self.config.to_owned()));
    router.delete(&self.endpoint, ResettableHandler::<R>::new(self.config.to_owned()));

    match env::var("DYNO") { // for some reasons, chain::link makes heroku crash
      Ok(_)  => Iron::new(router).http(&*host),
      Err(_) => {
        let mut chain = Chain::new(router);
        chain.link(HTTPLogger::new(None));
        Iron::new(chain).http(&*host)
      }
    }.unwrap();
  }
}
