package server

import "github.com/tinyshed/tinystore/server/wire"

// handlers is every method this server has, by its number
func (s *Server) handlers() map[wire.Method]handler {
	methods := map[wire.Method]handler{}
	s.kvMethods(methods)
	s.jobsMethods(methods)
	s.blobsMethods(methods)
	s.sqlMethods(methods)
	s.recordsMethods(methods)
	s.metricsMethods(methods)
	return methods
}
