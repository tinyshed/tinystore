package server

import (
	"fmt"

	"github.com/tinyshed/tinystore/server/wire"
)

// handlers is every method this server has, by its number
func (s *Server) handlers() map[wire.Method]handler {
	methods := map[wire.Method]handler{}
	s.kvMethods(methods)
	s.jobsMethods(methods)
	s.blobsMethods(methods)
	s.sqlMethods(methods)
	s.recordsMethods(methods)
	s.metricsMethods(methods)
	methods[wire.ServerStop] = serverStop
	return methods
}

// serverStop answers an admin's request to stop, then asks the program serving
// the store to stop, which only a program that said how can
func serverStop(c *call) error {
	var ask wire.Empty
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	stop := c.session.server.options.Stop
	switch {
	case c.session.capability != wire.Admin:
		return fmt.Errorf("%w: server: a stop", errAdminOnly)
	case stop == nil:
		return errStopsWithItsProgram
	}
	if err := respond(c, wire.Empty{}); err != nil {
		return err
	}
	c.session.server.log.Info("stopping", "asked by", c.session.client)
	stop()
	return nil
}
